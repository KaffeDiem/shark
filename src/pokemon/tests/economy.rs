use super::super::*;

fn wallet(used: u64, spent: u64, candies: u64) -> Companion {
    Companion {
        lifetime_tokens: used,
        spent_tokens: spent,
        inventory: Inventory {
            candies,
            ..Default::default()
        },
        random_seed: 7,
        ..Default::default()
    }
}
pub(super) fn line(forms: usize) -> EvoLine {
    let mut tree = EvoNode {
        species_id: forms as u16,
        children: vec![],
    };
    for id in (1..forms).rev() {
        tree = EvoNode {
            species_id: id as u16,
            children: vec![tree],
        };
    }
    EvoLine {
        base_id: 1,
        tree,
        rarity: Rarity::Common,
        names: (1..=forms)
            .map(|id| (id as u16, [("en".into(), format!("P{id}"))].into()))
            .collect(),
    }
}
pub(super) fn hatched() -> Companion {
    let mut s = wallet(5_000_000_000, 0, 0);
    s.egg_tokens = Balance::EGG_HATCH_THRESHOLD;
    s.hatch(line(3));
    assert!(s.active.is_some());
    s
}
fn roundtrip(s: &Companion) -> Companion {
    serde_json::from_slice(&serde_json::to_vec(s).unwrap()).unwrap()
}

// upstream: ShopTests.swift::testAvailableEqualsUsedWhenNothingSpent
#[test]
fn testAvailableEqualsUsedWhenNothingSpent() {
    assert_eq!(wallet(1_000_000_000, 0, 0).balance(), 1_000_000_000);
}
// upstream: ShopTests.swift::testAvailableSubtractsSpent
#[test]
fn testAvailableSubtractsSpent() {
    assert_eq!(wallet(1_000_000_000, 300_000_000, 0).balance(), 700_000_000);
}
// upstream: ShopTests.swift::testAvailableNeverNegative
#[test]
fn testAvailableNeverNegative() {
    assert_eq!(wallet(100_000_000, 500_000_000, 0).balance(), 0);
}
// upstream: ShopTests.swift::testDecodesWithoutSpentTokens
#[test]
fn testDecodesWithoutSpentTokens() {
    let s: Companion = serde_json::from_str(r#"{"lifetime_tokens":900}"#).unwrap();
    assert_eq!(s.spent_tokens, 0);
    assert_eq!(s.lifetime_tokens, 900);
}
// upstream: ShopTests.swift::testSpentTokensRoundTrip
#[test]
fn testSpentTokensRoundTrip() {
    assert_eq!(roundtrip(&wallet(1000, 400, 0)).spent_tokens, 400);
}
// upstream: ShopTests.swift::testCanBuyAtExactPrice
#[test]
fn testCanBuyAtExactPrice() {
    assert!(
        wallet(Item::RareCandy.price(), 0, 0)
            .buy(Item::RareCandy, 1)
            .is_ok()
    );
}
// upstream: ShopTests.swift::testCannotBuyOneBelowPrice
#[test]
fn testCannotBuyOneBelowPrice() {
    assert!(
        wallet(Item::RareCandy.price() - 1, 0, 0)
            .buy(Item::RareCandy, 1)
            .is_err()
    );
}
// upstream: ShopTests.swift::testBuyDebitsWalletAndCreditsInventory
#[test]
fn testBuyDebitsWalletAndCreditsInventory() {
    let mut s = wallet(1_000_000_000, 0, 0);
    s.buy(Item::RareCandy, 1).unwrap();
    assert_eq!(s.inventory.candies, 1);
    assert_eq!(s.spent_tokens, 500_000_000);
    assert_eq!(s.balance(), 500_000_000);
    assert_eq!(s.lifetime_tokens, 1_000_000_000);
}
// upstream: ShopTests.swift::testBuyInsufficientIsNoOp
#[test]
fn testBuyInsufficientIsNoOp() {
    let mut s = wallet(400_000_000, 0, 0);
    assert!(s.buy(Item::RareCandy, 1).is_err());
    assert_eq!(s.inventory.candies, 0);
    assert_eq!(s.spent_tokens, 0);
}
// upstream: ShopTests.swift::testMultipleBuysUntilBroke
#[test]
fn testMultipleBuysUntilBroke() {
    let mut s = wallet(1_200_000_000, 0, 0);
    assert!(s.buy(Item::RareCandy, 1).is_ok());
    assert!(s.buy(Item::RareCandy, 1).is_ok());
    assert!(s.buy(Item::RareCandy, 1).is_err());
    assert_eq!(s.inventory.candies, 2);
    assert_eq!(s.spent_tokens, 1_000_000_000);
    assert_eq!(s.balance(), 200_000_000);
}
// upstream: ShopTests.swift::testBuyAddsToExistingStock
#[test]
fn testBuyAddsToExistingStock() {
    let mut s = wallet(1_000_000_000, 0, 3);
    s.buy(Item::RareCandy, 1).unwrap();
    assert_eq!(s.inventory.candies, 4);
}
// upstream: ShopTests.swift::testBuyPersistsAcrossRestart
#[test]
fn testBuyPersistsAcrossRestart() {
    let mut s = wallet(1_000_000_000, 0, 0);
    s.buy(Item::RareCandy, 1).unwrap();
    let s = roundtrip(&s);
    assert_eq!(s.inventory.candies, 1);
    assert_eq!(s.spent_tokens, 500_000_000);
    assert_eq!(s.balance(), 500_000_000);
}
// upstream: ShopTests.swift::testBuyCountDebitsTotalAndCreditsInventory
#[test]
fn testBuyCountDebitsTotalAndCreditsInventory() {
    let mut s = wallet(2_000_000_000, 0, 1);
    s.buy(Item::RareCandy, 3).unwrap();
    assert_eq!(s.inventory.candies, 4);
    assert_eq!(s.spent_tokens, 1_500_000_000);
    assert_eq!(s.balance(), 500_000_000);
    assert_eq!(s.lifetime_tokens, 2_000_000_000);
}
// upstream: ShopTests.swift::testBuyCountAboveAffordableIsNoOp
#[test]
fn testBuyCountAboveAffordableIsNoOp() {
    let mut s = wallet(1_200_000_000, 0, 0);
    assert!(s.buy(Item::RareCandy, 3).is_err());
    assert_eq!(s.inventory.candies, 0);
    assert_eq!(s.spent_tokens, 0);
}
// upstream: ShopTests.swift::testBuyCountPersistsAcrossRestart
#[test]
fn testBuyCountPersistsAcrossRestart() {
    let mut s = wallet(1_000_000_000, 0, 0);
    s.buy(Item::Mint, 4).unwrap();
    let s = roundtrip(&s);
    assert_eq!(s.inventory.mints, 4);
    assert_eq!(s.spent_tokens, 400_000_000);
}

// upstream: MintTests.swift::testUseMintChangesNatureToDifferent
#[test]
fn testUseMintChangesNatureToDifferent() {
    let mut s = hatched();
    s.active.as_mut().unwrap().nature = "Adamant".into();
    s.inventory.mints = 1;
    s.use_mint().unwrap();
    assert_ne!(s.active.as_ref().unwrap().nature, "Adamant");
    assert_eq!(s.inventory.mints, 0);
}
// upstream: MintTests.swift::testMintNeverRepeatsCurrentAcrossUses
#[test]
fn testMintNeverRepeatsCurrentAcrossUses() {
    let mut s = hatched();
    s.inventory.mints = 6;
    for _ in 0..6 {
        let before = s.active.as_ref().unwrap().nature.clone();
        s.use_mint().unwrap();
        assert_ne!(s.active.as_ref().unwrap().nature, before);
    }
}
// upstream: MintTests.swift::testUseMintDoesNotAffectGrowthOrIdentity
#[test]
fn testUseMintDoesNotAffectGrowthOrIdentity() {
    let mut s = hatched();
    s.inventory.mints = 1;
    let mon = s.active.as_mut().unwrap();
    mon.tokens = 50_000_000;
    mon.shiny = true;
    let before = mon.clone();
    let used = s.lifetime_tokens;
    s.use_mint().unwrap();
    let after = s.active.as_ref().unwrap();
    assert_eq!(after.tokens, 50_000_000);
    assert_eq!(after.stage, before.stage);
    assert_eq!(after.species_id(), before.species_id());
    assert!(after.shiny);
    assert_eq!(s.lifetime_tokens, used);
}
// upstream: MintTests.swift::testCannotUseMintOnEgg
#[test]
fn testCannotUseMintOnEgg() {
    let mut s = Companion::default();
    s.inventory.mints = 2;
    assert!(s.use_mint().is_err());
    assert_eq!(s.inventory.mints, 2);
}
// upstream: MintTests.swift::testCannotUseMintWithoutStock
#[test]
fn testCannotUseMintWithoutStock() {
    let mut s = hatched();
    assert!(s.use_mint().is_err());
}
// upstream: MintTests.swift::testUseMintPersistsAcrossRestart
#[test]
fn testUseMintPersistsAcrossRestart() {
    let mut s = hatched();
    s.inventory.mints = 2;
    s.use_mint().unwrap();
    let next = roundtrip(&s);
    assert_eq!(
        next.active.as_ref().unwrap().nature,
        s.active.as_ref().unwrap().nature
    );
    assert_eq!(next.inventory.mints, 1);
}
// upstream: MintTests.swift::testBuyMintDebitsWalletAndCredits
#[test]
fn testBuyMintDebitsWalletAndCredits() {
    let mut s = wallet(300_000_000, 0, 0);
    s.buy(Item::Mint, 1).unwrap();
    assert_eq!(s.inventory.mints, 1);
    assert_eq!(s.spent_tokens, 100_000_000);
    assert_eq!(s.balance(), 200_000_000);
}
// upstream: MintTests.swift::testCannotBuyMintBelowPrice
#[test]
fn testCannotBuyMintBelowPrice() {
    let mut s = wallet(Item::Mint.price() - 1, 0, 0);
    assert!(s.buy(Item::Mint, 1).is_err());
    assert_eq!(s.inventory.mints, 0);
}

// upstream: FreshEggTests.swift::testBuyFreshEggReleasesIntoDexWithoutProbabilityImpact
#[test]
fn testBuyFreshEggReleasesIntoDexWithoutProbabilityImpact() {
    let mut s = hatched();
    let count = s.collection.len();
    s.fresh_egg(None).unwrap();
    assert!(s.active.is_none());
    assert_eq!(s.egg_tokens, 0);
    assert_eq!(s.collection.len(), count + 1);
    let released = s.collection.last().unwrap();
    assert!(released.released);
    assert!(!released.graduated);
    assert_eq!(released.path, [1]);
    assert_eq!(s.spent_tokens, 1_000_000_000);
    assert_eq!(s.balance(), 4_000_000_000);
}
// upstream: FreshEggTests.swift::testReleasedSpeciesStaysInTheDex
#[test]
fn testReleasedSpeciesStaysInTheDex() {
    let mut s = hatched();
    assert!(
        s.active
            .as_ref()
            .unwrap()
            .reached_species()
            .any(|id| id == 1)
    );
    s.fresh_egg(None).unwrap();
    assert!(
        s.collection
            .iter()
            .any(|m| m.reached_species().any(|id| id == 1))
    );
    assert!(s.active.is_none());
}
// upstream: FreshEggTests.swift::testDiscardedSpeciesNotCollected
#[test]
fn testDiscardedSpeciesNotCollected() {
    let mut s = hatched();
    s.fresh_egg(None).unwrap();
    assert!(
        !s.collection
            .iter()
            .any(|m| m.line.base_id == 1 && m.graduated)
    );
    s.egg_tokens = Balance::EGG_HATCH_THRESHOLD;
    s.hatch(line(3));
    assert!(!s.active.as_ref().unwrap().growth_boost);
}
// upstream: FreshEggTests.swift::testCannotRerollWhenEgg
#[test]
fn testCannotRerollWhenEgg() {
    let mut s = wallet(5_000_000_000, 0, 0);
    assert!(s.fresh_egg(None).is_err());
    assert_eq!(s.spent_tokens, 0);
}
// upstream: FreshEggTests.swift::testCannotRerollWithoutFunds
#[test]
fn testCannotRerollWithoutFunds() {
    let mut s = hatched();
    s.lifetime_tokens = 500_000_000;
    assert!(s.fresh_egg(None).is_err());
    assert!(s.active.is_some());
    assert_eq!(s.spent_tokens, 0);
}
// upstream: FreshEggTests.swift::testShinyCanBeRerolled
#[test]
fn testShinyCanBeRerolled() {
    let mut s = hatched();
    s.active.as_mut().unwrap().shiny = true;
    s.fresh_egg(None).unwrap();
    assert!(s.active.is_none());
    assert_eq!(s.representative_subject(), None);
}

// upstream: ShinyCharmTests.swift::testBuyDeductsAndOwns
#[test]
fn testBuyDeductsAndOwns() {
    let mut s = wallet(5_000_000_000, 0, 0);
    assert!(!s.inventory.shiny_charm);
    s.buy(Item::ShinyCharm, 1).unwrap();
    assert!(s.inventory.shiny_charm);
    assert_eq!(s.spent_tokens, 3_000_000_000);
    assert_eq!(s.balance(), 2_000_000_000);
}
// upstream: ShinyCharmTests.swift::testBuyOnceNoRepurchase
#[test]
fn testBuyOnceNoRepurchase() {
    let mut s = wallet(10_000_000_000, 0, 0);
    s.inventory.shiny_charm = true;
    assert!(s.buy(Item::ShinyCharm, 1).is_err());
    assert_eq!(s.spent_tokens, 0);
    assert!(s.inventory.shiny_charm);
}
// upstream: ShinyCharmTests.swift::testCanBuyNeedsEnoughTokens
#[test]
fn testCanBuyNeedsEnoughTokens() {
    let mut s = wallet(2_000_000_000, 0, 0);
    assert!(s.buy(Item::ShinyCharm, 1).is_err());
    assert!(!s.inventory.shiny_charm);
}
// upstream: ShinyCharmTests.swift::testOwnsReflectsInventory
#[test]
fn testOwnsReflectsInventory() {
    let mut s = wallet(5_000_000_000, 0, 0);
    assert!(!s.inventory.shiny_charm);
    s.inventory.shiny_charm = true;
    assert!(roundtrip(&s).inventory.shiny_charm);
}
// upstream: ShinyCharmTests.swift::testHatchWorksWithCharmOwned
#[test]
fn testHatchWorksWithCharmOwned() {
    let mut s = wallet(5_000_000_000, 0, 0);
    s.inventory.shiny_charm = true;
    s.egg_tokens = Balance::EGG_HATCH_THRESHOLD;
    s.hatch(line(1));
    assert_eq!(s.active.as_ref().unwrap().species_id(), 1);
}

#[test]
fn zero_and_overflowing_quantities_do_not_mutate_wallet() {
    let mut s = wallet(u64::MAX, 0, 0);
    for amount in [0, u64::MAX] {
        assert!(s.buy(Item::RareCandy, amount).is_err());
        assert_eq!(s.inventory.candies, 0);
        assert_eq!(s.spent_tokens, 0);
    }
}

// upstream: ShopTests.swift::testMaxBuyCountIsFloorOfWalletOverPrice
#[test]
fn testMaxBuyCountIsFloorOfWalletOverPrice() {
    let s = wallet(1_200_000_000, 0, 0);
    assert_eq!(s.max_buy_count(Item::RareCandy), 2);
    assert_eq!(s.max_buy_count(Item::Mint), 12);
    assert_eq!(
        wallet(Item::RareCandy.price() - 1, 0, 0).max_buy_count(Item::RareCandy),
        0
    );
}
// upstream: ShopTests.swift::testMaxBuyCountCapsPassiveAtOne
#[test]
fn testMaxBuyCountCapsPassiveAtOne() {
    let mut s = wallet(10 * Item::ShinyCharm.price(), 0, 0);
    assert_eq!(s.max_buy_count(Item::ShinyCharm), 1);
    assert!(s.buy(Item::ShinyCharm, 2).is_err());
    assert_eq!(s.spent_tokens, 0);
    s.buy(Item::ShinyCharm, 1).unwrap();
    assert_eq!(s.max_buy_count(Item::ShinyCharm), 0);
}
// upstream: ShopTests.swift::testBuyExactlyMaxCountEmptiesWallet
#[test]
fn testBuyExactlyMaxCountEmptiesWallet() {
    let mut s = wallet(3 * Item::RareCandy.price(), 0, 0);
    s.buy(Item::RareCandy, s.max_buy_count(Item::RareCandy))
        .unwrap();
    assert_eq!(s.inventory.candies, 3);
    assert_eq!(s.balance(), 0);
}
// upstream: ShopTests.swift::testItemsSortedByPriceAscending
#[test]
fn testItemsSortedByPriceAscending() {
    let s = wallet(0, 0, 0);
    assert_eq!(
        s.purchasable_items(),
        [Item::Mint, Item::RareCandy, Item::ShinyCharm]
    );
    assert!(
        s.purchasable_items()
            .windows(2)
            .all(|w| w[0].price() <= w[1].price())
    );
}
// upstream: ShopTests.swift::testOwnedPassiveSinksToBottom
#[test]
fn testOwnedPassiveSinksToBottom() {
    let mut s = wallet(0, 0, 0);
    s.inventory.shiny_charm = true;
    assert_eq!(s.purchasable_items().last(), Some(&Item::ShinyCharm));
    assert_eq!(
        s.shop_entries().last(),
        Some(&ShopEntry::Item(Item::ShinyCharm))
    );
}
// upstream: ShopTests.swift::testShopEntriesInterleavesFreshEggByPrice
#[test]
fn testShopEntriesInterleavesFreshEggByPrice() {
    let s = hatched();
    assert_eq!(
        s.shop_entries(),
        [
            ShopEntry::Item(Item::Mint),
            ShopEntry::Item(Item::RareCandy),
            ShopEntry::Egg(None),
            ShopEntry::Egg(Some(Rarity::Uncommon)),
            ShopEntry::Item(Item::ShinyCharm),
            ShopEntry::Egg(Some(Rarity::Rare))
        ]
    );
}
// upstream: ShopTests.swift::testShopEntriesKeepsEggsVisibleButUnbuyableWhenNoActive
#[test]
fn testShopEntriesKeepsEggsVisibleButUnbuyableWhenNoActive() {
    let mut s = wallet(5_000_000_000, 0, 0);
    assert_eq!(s.shop_entries(), hatched().shop_entries());
    for tier in [None, Some(Rarity::Uncommon), Some(Rarity::Rare)] {
        assert!(s.shop_entries().contains(&ShopEntry::Egg(tier)));
        assert!(!s.can_buy_egg(tier));
        assert!(s.fresh_egg(tier).is_err());
    }
    assert_eq!(s.balance(), 5_000_000_000);
}
// upstream: MintTests.swift::testMintShopPriceAndPurchasable
#[test]
fn testMintShopPriceAndPurchasable() {
    assert_eq!(Item::Mint.price(), 100_000_000);
    let s = hatched();
    assert!(s.purchasable_items().contains(&Item::RareCandy));
    assert!(s.purchasable_items().contains(&Item::Mint));
}
// upstream: ShopTests.swift::testBuyNonPositiveCountIsNoOp
#[test]
fn testBuyNonPositiveCountIsNoOp() {
    let mut s = wallet(1_000_000_000, 0, 0);
    assert!(s.buy(Item::RareCandy, 0).is_err());
    assert!(
        serde_json::from_str::<u64>("-1").is_err(),
        "negative quantities are rejected by Rust's unsigned boundary"
    );
    assert_eq!(s.inventory.candies, 0);
    assert_eq!(s.spent_tokens, 0);
}
// upstream: MintTests.swift::testUseMintFromNilNatureSetsValid
#[test]
fn testUseMintFromNilNatureSetsValid() {
    let mut s = hatched();
    s.active.as_mut().unwrap().nature.clear();
    s.inventory.mints = 1;
    s.use_mint().unwrap();
    assert!(NATURES.contains(&s.active.as_ref().unwrap().nature.as_str()));
}
// upstream: MintTests.swift::testMintFeedbackSetAndConsumed
#[test]
fn testMintFeedbackSetAndConsumed() {
    let mut s = hatched();
    s.inventory.mints = 1;
    let before = s.feedback.mint_seq;
    s.use_mint().unwrap();
    assert_eq!(
        s.feedback.mint_nature.as_deref(),
        Some(s.active.as_ref().unwrap().nature.as_str())
    );
    assert_eq!(s.feedback.mint_seq, before + 1);
    s.consume_mint_feedback();
    assert_eq!(s.feedback.mint_nature, None);
}
// upstream: ShinyCharmTests.swift::testRollsShinyFlipsWithCharm
#[test]
fn testRollsShinyFlipsWithCharm() {
    for (roll, charm, expected) in [
        (48, true, true),
        (48, false, false),
        (64, false, true),
        (64, true, false),
        (96, true, true),
        (96, false, false),
        (0, true, true),
        (0, false, true),
        (1, true, false),
        (1, false, false),
    ] {
        assert_eq!(Companion::rolls_shiny(roll, charm), expected);
    }
}
// upstream: ShinyCharmTests.swift::testConstantsAndPassiveFlag
#[test]
fn testConstantsAndPassiveFlag() {
    assert_eq!(Item::ShinyCharm.price(), 3_000_000_000);
    let mut s = Companion::default();
    s.inventory.shiny_charm = true;
    assert_eq!(s.shiny_denominator(), 48);
    assert!(Item::ShinyCharm.is_passive());
    assert!(!Item::RareCandy.is_passive());
    assert!(!Item::Mint.is_passive());
    assert_eq!(Item::ShinyCharm.sprite_name(), Some("shiny-charm"));
}
// upstream: ShinyCharmTests.swift::testShinyNotificationShowsTheOddsTheHatchUsed
#[test]
fn testShinyNotificationShowsTheOddsTheHatchUsed() {
    let mut s = Companion::default();
    assert_eq!(s.shiny_denominator(), 64);
    s.inventory.shiny_charm = true;
    assert_eq!(s.shiny_denominator(), 48);
    for lang in format::Language::ALL {
        let body = lang.shiny_hatch("P", s.shiny_denominator());
        assert!(body.contains("48"));
        assert!(!body.contains("64"));
    }
}
