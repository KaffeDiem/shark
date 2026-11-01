//! Behavioral migrations of upstream CompanionTests, DittoTests and UnownTests.
#![allow(non_snake_case)]
use super::*;
fn node(id: u16, children: Vec<EvoNode>) -> EvoNode {
    EvoNode {
        species_id: id,
        children,
    }
}
fn line(ids: &[u16], rarity: Rarity) -> EvoLine {
    let mut tree = node(*ids.last().unwrap(), vec![]);
    for &id in ids.iter().rev().skip(1) {
        tree = node(id, vec![tree]);
    }
    EvoLine {
        base_id: ids[0],
        tree,
        rarity,
        names: ids
            .iter()
            .map(|&id| {
                (
                    id,
                    BTreeMap::from([
                        ("en".into(), format!("P{id}")),
                        ("de".into(), format!("D{id}")),
                    ]),
                )
            })
            .collect(),
    }
}
fn companion(ids: &[u16], rarity: Rarity) -> Companion {
    let mut s = Companion {
        random_seed: 7,
        egg_tokens: Balance::EGG_HATCH_THRESHOLD,
        ..Default::default()
    };
    s.hatch_with_disguise_roll(line(ids, rarity), false);
    s
}
fn roundtrip(s: &Companion) -> Companion {
    serde_json::from_slice(&serde_json::to_vec(s).unwrap()).unwrap()
}
fn disguise() -> Companion {
    let mut s = companion(&[1, 2, 3], Rarity::Common);
    s.active.as_mut().unwrap().ditto_disguise = Some(1);
    s
}
#[test]
fn testInstallBaselineExcludesPreInstallUsage() {
    let mut s = Companion::default();
    s.observe_usage("claude", "2026-01-01", 900_000_000, true);
    assert_eq!(s.egg_tokens, 0);
    s.observe_usage("claude", "2026-01-01", 901_000_000, true);
    assert_eq!(s.egg_tokens, 1_000_000);
    assert_eq!(s.lifetime_tokens, 1_000_000);
}
#[test]
fn testProviderLedgerDoesNotRecreditMissingProviderAfterPartialSnapshotLoss() {
    let mut s = Companion::default();
    for p in ["claude", "codex"] {
        s.observe_usage(p, "d1", 100, true);
    }
    s.observe_usage("claude", "d1", 150, true);
    s.observe_usage("claude", "d1", 160, true);
    s.observe_usage("codex", "d1", 100, true);
    assert_eq!(s.lifetime_tokens, 60);
}
#[test]
fn testDateRolloverCreditsCurrentDayUsage() {
    let mut s = Companion::default();
    s.observe_usage("claude", "d1", 100, true);
    s.observe_usage("claude", "d2", 50, true);
    assert_eq!(s.lifetime_tokens, 50);
}
#[test]
fn testLateProviderRecoveryAfterDateRolloverCreditsCurrentDayUsage() {
    let mut s = Companion::default();
    for p in ["claude", "codex"] {
        s.observe_usage(p, "d1", 100, true);
    }
    s.observe_usage("claude", "d2", 30, true);
    s.observe_usage("codex", "d2", 40, true);
    assert_eq!(s.lifetime_tokens, 70);
}
#[test]
fn testStaleSnapshotDoesNotConsumeDateBoundary() {
    let mut s = Companion::default();
    s.observe_usage("claude", "d1", 100, true);
    s.observe_usage("claude", "d2", 30, true);
    s.observe_usage("claude", "d1", 300, true);
    assert_eq!(s.lifetime_tokens, 30);
    s.observe_usage("claude", "d2", 40, true);
    assert_eq!(s.lifetime_tokens, 40);
}
#[test]
fn testEggDoesNotHatchBelowThreshold() {
    let mut s = Companion {
        egg_tokens: 4_999_999,
        ..Default::default()
    };
    assert!(s.hatch(line(&[1, 2, 3], Rarity::Common)).is_empty());
    assert!(s.active.is_none());
}
#[test]
fn testEggHatchesAtThreshold() {
    let s = companion(&[1, 2, 3], Rarity::Common);
    assert_eq!(s.active.unwrap().species_id(), 1);
    assert_eq!(s.egg_tokens, 0);
}
#[test]
fn testEggOverflowCarriesToHatchedMon() {
    let mut s = Companion {
        egg_tokens: 6_000_000,
        ..Default::default()
    };
    s.hatch(line(&[1, 2, 3], Rarity::Common));
    assert_eq!(s.active.unwrap().tokens, 1_000_000);
}
#[test]
fn testNewEggAfterGraduationReincubates() {
    let mut s = companion(&[1], Rarity::Common);
    s.add_growth(800_000_000);
    assert!(s.active.is_none());
    assert_eq!(s.egg_tokens, 0);
    assert!(!s.ready_to_hatch());
}
#[test]
fn testStateDecodesWithoutEggUsage() {
    let s: Companion = serde_json::from_str("{}").unwrap();
    assert_eq!(s.egg_tokens, 0);
    assert!(s.active.is_none());
}
#[test]
fn testEvolvesThroughLineAndGraduatesWithFullChain() {
    let mut s = companion(&[1, 2, 3], Rarity::Common);
    assert_eq!(s.add_growth(125_000_000), vec![Event::Evolved("P2".into())]);
    assert_eq!(s.add_growth(250_000_000), vec![Event::Evolved("P3".into())]);
    assert_eq!(
        s.add_growth(375_000_000),
        vec![Event::Graduated("P3".into())]
    );
    assert!(s.active.is_none());
    assert_eq!(s.collection[0].path, vec![1, 2, 3]);
    assert!(s.collection[0].graduated);
}
#[test]
fn testNoEvolutionGraduatesAtSingleThreshold() {
    let mut s = companion(&[25], Rarity::Common);
    assert!(s.add_growth(749_999_999).is_empty());
    assert_eq!(s.add_growth(1), vec![Event::Graduated("P25".into())]);
}
#[test]
fn testPersistenceRoundTrip() {
    let mut s = companion(&[1, 2, 3], Rarity::Rare);
    s.add_growth(20_000_000);
    s.inventory.candies = 4;
    s.inventory.mints = 2;
    let restored = roundtrip(&s);
    assert_eq!(restored.progress(), s.progress());
    assert_eq!(restored.inventory.candies, 4);
    assert_eq!(restored.title(), s.title());
    assert_eq!(
        restored.active.as_ref().unwrap().nature,
        s.active.as_ref().unwrap().nature
    );
}
#[test]
fn testRepeatGrowthPersistsAcrossRestartWhileLegacyActiveDefaultsToStandardGrowth() {
    let mut s = companion(&[1, 2, 3], Rarity::Common);
    s.active.as_mut().unwrap().growth_boost = true;
    assert_eq!(roundtrip(&s).progress().1, 62_500_000);
    let mut value = serde_json::to_value(&s).unwrap();
    value["active"]
        .as_object_mut()
        .unwrap()
        .remove("growth_boost");
    let legacy: Companion = serde_json::from_value(value).unwrap();
    assert_eq!(legacy.progress().1, 125_000_000);
}
#[test]
fn testHatchPreselectsWurmpleRouteAndEvolutionDoesNotConsumeRNG() {
    let mut s = Companion {
        egg_tokens: 5_000_000,
        random_seed: 17,
        ..Default::default()
    };
    let mut l = line(&[265, 266, 267], Rarity::Common);
    l.tree.children.push(node(268, vec![node(269, vec![])]));
    s.hatch(l);
    let seed = s.random_seed;
    let planned = s.active.as_ref().unwrap().path.clone();
    s.add_growth(125_000_000);
    assert_eq!(s.random_seed, seed);
    assert_eq!(s.active.as_ref().unwrap().species_id(), planned[1]);
}
#[test]
fn testRepeatGrowthIsDecidedFromTheCollectedBaseNotThePlannedFinal() {
    let mut s = companion(&[265, 266, 267], Rarity::Common);
    s.add_growth(750_000_000);
    s.egg_tokens = 5_000_000;
    let mut l = line(&[265, 266, 267], Rarity::Common);
    l.tree.children.push(node(268, vec![node(269, vec![])]));
    s.hatch(l);
    assert_eq!(s.active.as_ref().unwrap().path, vec![265, 268, 269]);
    assert!(s.active.unwrap().growth_boost);
}
#[test]
fn testBranchingPrefersUncollectedFinals() {
    for seed in 0..100 {
        let mut s = companion(&[265, 266, 267], Rarity::Common);
        s.add_growth(750_000_000);
        s.egg_tokens = 5_000_000;
        s.random_seed = seed;
        let mut l = line(&[265, 266, 267], Rarity::Common);
        l.tree.children.push(node(268, vec![node(269, vec![])]));
        s.hatch(l);
        assert_eq!(s.active.unwrap().path, vec![265, 268, 269]);
    }
}
#[test]
fn testAsymmetricBranchGraduatesSafely() {
    let mut l = line(&[1, 2, 3], Rarity::Common);
    l.tree.children.push(node(4, vec![]));
    for seed in 0..30 {
        let mut s = Companion {
            egg_tokens: 5_000_000,
            random_seed: seed,
            ..Default::default()
        };
        s.hatch(l.clone());
        let count = s.active.as_ref().unwrap().path.len();
        assert!([2, 3].contains(&count));
        s.add_growth(750_000_000);
        assert!(s.active.is_none());
        assert_eq!(s.collection.len(), 1);
    }
}
#[test]
fn testGraduateCarriesIdentityToDex() {
    let mut s = companion(&[1, 2, 3], Rarity::Common);
    let m = s.active.clone().unwrap();
    s.add_growth(750_000_000);
    let d = &s.collection[0];
    assert_eq!(d.shiny, m.shiny);
    assert_eq!(d.nature, m.nature);
    assert_eq!(
        d.profile.as_ref().unwrap().instance_id,
        m.profile.unwrap().instance_id
    );
    assert_eq!(d.profile.as_ref().unwrap().level, 100);
}
#[test]
fn testHatchAssignsDeterministicShinyAndNature() {
    let a = companion(&[1, 2, 3], Rarity::Common);
    let b = companion(&[1, 2, 3], Rarity::Common);
    assert_eq!(
        a.active.as_ref().unwrap().shiny,
        b.active.as_ref().unwrap().shiny
    );
    assert_eq!(
        a.active.as_ref().unwrap().nature,
        b.active.as_ref().unwrap().nature
    );
    assert!(NATURES.contains(&a.active.unwrap().nature.as_str()));
}
#[test]
fn testShinyPathReachable() {
    assert!((0..1000).any(|seed| {
        let mut s = Companion {
            egg_tokens: 5_000_000,
            random_seed: seed,
            ..Default::default()
        };
        s.hatch(line(&[25], Rarity::Rare));
        s.active.unwrap().shiny
    }));
}
#[test]
fn testCurrentIDFallsBackToBaseWhenPathEmpty() {
    let mut s = companion(&[25], Rarity::Common);
    s.active.as_mut().unwrap().path.clear();
    assert_eq!(s.active.unwrap().species_id(), 25);
}
#[test]
fn testCelebrationFiresOnHatchAndEvolve() {
    let mut s = Companion {
        egg_tokens: 5_000_000,
        ..Default::default()
    };
    assert_eq!(
        s.hatch(line(&[1, 2, 3], Rarity::Common)),
        vec![Event::Hatched("P1".into())]
    );
    assert_eq!(s.add_growth(125_000_000), vec![Event::Evolved("P2".into())]);
}
#[test]
fn testValidatedChainURLAcceptsPokeapiHttps() {
    assert_eq!(
        api::validated_chain_id("https://pokeapi.co/api/v2/evolution-chain/123/"),
        Some(123)
    );
}
#[test]
fn testValidatedChainURLRejectsUntrusted() {
    for value in [
        "",
        "http://pokeapi.co/api/v2/evolution-chain/1/",
        "https://evil.example/api/v2/evolution-chain/1/",
        "https://pokeapi.co.evil.example/api/v2/evolution-chain/1/",
        "file:///api/v2/evolution-chain/1/",
        "https://pokeapi.co/api/v2/pokemon/1/",
        "https://user:pass@pokeapi.co/api/v2/evolution-chain/1/",
    ] {
        assert_eq!(api::validated_chain_id(value), None, "{value}");
    }
}
#[test]
fn testSamplerCaptureRateIsWeight() {
    let entries = [
        api::BaseSpecies {
            id: 100,
            capture_rate: 254,
        },
        api::BaseSpecies {
            id: 200,
            capture_rate: 2,
        },
    ];
    for roll in 0..256 {
        assert_eq!(
            api::weighted_pick(&entries, &Default::default(), roll),
            Some(if roll < 254 { 100 } else { 200 })
        );
    }
}
#[test]
fn testSamplerHalvesCollectedWeight() {
    let entries = [
        api::BaseSpecies {
            id: 1,
            capture_rate: 200,
        },
        api::BaseSpecies {
            id: 2,
            capture_rate: 200,
        },
    ];
    let owned = std::collections::BTreeSet::from([1]);
    let seed = (0..5000)
        .find(|&seed| {
            let mut rng = seed;
            let v = next_random(&mut rng);
            v % 400 < 200 && v % 300 >= 100
        })
        .unwrap();
    let mut rng = seed;
    let roll = next_random(&mut rng);
    assert_eq!(
        api::weighted_pick(&entries, &Default::default(), roll),
        Some(1)
    );
    assert_eq!(api::weighted_pick(&entries, &owned, roll), Some(2));
}

#[test]
fn testSamplerWeightedPickDeterministic() {
    let entries = [
        api::BaseSpecies {
            id: 10,
            capture_rate: 100,
        },
        api::BaseSpecies {
            id: 20,
            capture_rate: 100,
        },
        api::BaseSpecies {
            id: 30,
            capture_rate: 100,
        },
    ];
    for seed in [1, 7, 42, 999] {
        let mut rng = seed;
        let roll = next_random(&mut rng);
        assert_eq!(
            api::weighted_pick(&entries, &Default::default(), roll),
            Some(entries[(roll % 300 / 100) as usize].id)
        );
    }
}
#[test]
fn testHitCommonMultiFormOnMultipleOfDenominator() {
    for (forms, roll) in [(2, 0), (3, 128), (3, 256)] {
        assert!(ditto::disguise_hit(Rarity::Common, forms, roll));
    }
}
#[test]
fn testMissWhenRollNotMultipleOfDenominator() {
    for roll in [1, 127, 129] {
        assert!(!ditto::disguise_hit(Rarity::Common, 3, roll));
    }
}
#[test]
fn testExcludesSingleForm() {
    assert!(!ditto::disguise_hit(Rarity::Common, 1, 0));
}
#[test]
fn testExcludesNonCommon() {
    for rarity in [Rarity::Uncommon, Rarity::Rare, Rarity::Legendary] {
        assert!(!ditto::disguise_hit(rarity, 3, 0));
    }
}
#[test]
fn testDenominatorIs128() {
    assert_eq!(ditto::DENOMINATOR, 128);
}
#[test]
fn testShinyHiddenDuringDisguise() {
    let mut s = disguise();
    s.active.as_mut().unwrap().shiny = true;
    assert!(s.active.as_ref().unwrap().shiny);
    assert!(!s.active.as_ref().unwrap().displays_shiny());
    assert_eq!(s.representative_subject(), Some((1, false)));
}
#[test]
fn testRevealAtFirstEvolution() {
    let mut s = disguise();
    assert!(s.add_growth(300_000_000).is_empty());
    assert_eq!(s.active.as_ref().unwrap().species_id(), 1);
    assert!(s.ready_to_reveal());
    s.reveal_ditto(line(&[132], Rarity::Rare));
    let mon = s.active.unwrap();
    assert_eq!(mon.line.base_id, 132);
    assert_eq!(mon.path, vec![132]);
    assert_eq!(mon.stage, 0);
    assert_eq!(mon.tokens, 175_000_000);
    assert_eq!(mon.line.rarity, Rarity::Rare);
    assert!(mon.ditto_revealed);
    assert_eq!(mon.ditto_disguise, Some(1));
}
#[test]
fn testBoostedDisguiseRevealsAtTheHalvedThresholdAndANewDittoDropsTheBoost() {
    let mut s = disguise();
    s.active.as_mut().unwrap().growth_boost = true;
    s.add_growth(62_500_000);
    s.reveal_ditto(line(&[132], Rarity::Rare));
    assert_eq!(s.progress(), (0, 3_000_000_000));
    assert!(!s.active.unwrap().growth_boost);
}
#[test]
fn testRevealingAnAlreadyGraduatedDittoGrantsTheRepeatBoost() {
    let mut s = disguise();
    let mut archived = companion(&[132], Rarity::Rare);
    archived.add_growth(3_000_000_000);
    s.collection = archived.collection;
    s.add_growth(125_000_000);
    s.reveal_ditto(line(&[132], Rarity::Rare));
    assert!(s.active.as_ref().unwrap().growth_boost);
    assert_eq!(s.progress().1, 1_500_000_000);
}
#[test]
fn testRevealClearsRepresentativePinnedToDisguiseAndFollowsDitto() {
    let mut s = disguise();
    s.active.as_mut().unwrap().shiny = true;
    assert!(s.pin(Some(1)));
    assert_eq!(s.representative_subject(), Some((1, false)));
    s.add_growth(300_000_000);
    s.reveal_ditto(line(&[132], Rarity::Rare));
    assert_eq!(s.representative, None);
    assert_eq!(s.representative_subject(), Some((132, true)));
}
#[test]
fn testPrunedLeafDisguiseRevealsBeforeGraduation() {
    let mut s = companion(&[206], Rarity::Common);
    s.active.as_mut().unwrap().ditto_disguise = Some(206);
    s.add_growth(750_000_000);
    assert!(s.ready_to_reveal());
    assert!(s.collection.is_empty());
    s.reveal_ditto(line(&[132], Rarity::Rare));
    assert_eq!(s.active.unwrap().species_id(), 132);
    assert!(s.collection.is_empty());
}
#[test]
fn testShinyUnmaskedAfterReveal() {
    let mut s = disguise();
    s.active.as_mut().unwrap().shiny = true;
    s.add_growth(125_000_000);
    s.reveal_ditto(line(&[132], Rarity::Rare));
    assert!(s.active.unwrap().displays_shiny());
}
#[test]
fn testNoRevealBelowThreshold() {
    let mut s = disguise();
    s.add_growth(124_999_999);
    assert!(!s.ready_to_reveal());
    assert!(s.reveal_ditto(line(&[132], Rarity::Rare)).is_empty());
    assert_eq!(s.active.unwrap().species_id(), 1);
}
#[test]
fn testApparentGraduationBatchPreservesUnusedCandiesAtDittoReveal() {
    let mut s = disguise();
    s.inventory.candies = 20;
    s.use_candy(8).unwrap();
    assert_eq!(s.inventory.candies, 18);
    assert!(s.ready_to_reveal());
    s.reveal_ditto(line(&[132], Rarity::Rare));
    assert_eq!(s.active.unwrap().tokens, 75_000_000);
}
#[test]
fn testCandyBatchStopsExactlyAtRevealWithoutOverflow() {
    let mut s = disguise();
    s.active.as_mut().unwrap().tokens = 25_000_000;
    s.inventory.candies = 20;
    s.use_candy(20).unwrap();
    assert_eq!(s.inventory.candies, 19);
    s.reveal_ditto(line(&[132], Rarity::Rare));
    assert_eq!(s.active.unwrap().tokens, 0);
}
#[test]
fn testBackwardCompatDecodeNoDittoFields() {
    let s = companion(&[1, 2, 3], Rarity::Common);
    let mut value = serde_json::to_value(&s).unwrap();
    let a = value["active"].as_object_mut().unwrap();
    a.remove("ditto_disguise");
    a.remove("ditto_revealed");
    let restored: Companion = serde_json::from_value(value).unwrap();
    assert_eq!(restored.active.as_ref().unwrap().ditto_disguise, None);
    assert!(!restored.active.unwrap().ditto_revealed);
}
#[test]
fn testBoostedEasyDittoRetainsLevelAndIdentityAcrossReveal() {
    let mut s = disguise();
    s.active.as_mut().unwrap().growth_boost = true;
    s.set_growth_difficulty(0.1);
    let initial = s.active.as_ref().unwrap().profile.clone().unwrap();
    s.add_growth(7_250_000);
    let level = s.active.as_ref().unwrap().profile.as_ref().unwrap().level;
    s.reveal_ditto(line(&[132], Rarity::Rare));
    let m = s.active.as_ref().unwrap();
    assert_eq!(m.tokens, 1_000_000);
    assert!(!m.growth_boost);
    assert_eq!(m.profile.as_ref().unwrap().instance_id, initial.instance_id);
    assert_eq!(m.profile.as_ref().unwrap().ivs, initial.ivs);
    assert_eq!(m.profile.as_ref().unwrap().level, level);
    s.add_growth(s.progress().1);
    assert_eq!(
        s.collection.last().unwrap().profile.as_ref().unwrap().level,
        100
    );
}
#[test]
fn testAllLettersAndPunctuationAreSelectable() {
    let fs = forms::forms(201);
    let symbols = (b'A'..=b'Z')
        .map(|c| (c as char).to_string())
        .chain(["!".into(), "?".into()])
        .collect::<Vec<_>>();
    assert_eq!(
        fs.iter().map(forms::Form::symbol).collect::<Vec<_>>(),
        symbols
    );
    for roll in 0..56 {
        assert_eq!(
            forms::roll(roll, &fs, &Default::default())
                .unwrap()
                .symbol(),
            symbols[(roll / 2) as usize]
        );
    }
    assert_eq!(
        forms::roll(56, &fs, &Default::default()),
        Some(forms::Form("a".into()))
    );
    assert!(fs.contains(&forms::roll(u64::MAX, &fs, &Default::default()).unwrap()));
}
#[test]
fn testFormWeightsAreTwoForMissingAndOneForCollectedWithoutGuaranteeingNewForms() {
    let fs = forms::forms(201);
    for owned in [
        Default::default(),
        std::collections::BTreeSet::from([
            forms::Form("a".into()),
            forms::Form("b".into()),
            forms::Form("exclamation".into()),
        ]),
        fs.iter().cloned().collect(),
    ] {
        let total = 56 - owned.len() as u64;
        let results = (0..total)
            .map(|r| forms::roll(r, &fs, &owned).unwrap())
            .collect::<Vec<_>>();
        for f in &fs {
            assert_eq!(
                results.iter().filter(|v| *v == f).count(),
                if owned.contains(f) { 1 } else { 2 }
            );
        }
    }
}
#[test]
fn testSharedCollectedAdjustmentPreservesCaptureRateRoundingAndMinimum() {
    for weight in 0..=255 {
        assert_eq!(api::adjusted_weight(weight, false), weight.max(1));
        assert_eq!(api::adjusted_weight(weight, true), (weight / 2).max(1));
    }
}
#[test]
fn testEveryFormRoundTripsAndSharesOneSpeciesCell() {
    let mut s = Companion::default();
    for form in forms::forms(201) {
        let mut m = companion(&[201], Rarity::Common).active.unwrap();
        m.form = Some(form);
        m.graduated = true;
        s.collection.push(m);
    }
    let restored = roundtrip(&s);
    assert_eq!(restored.collection.len(), 28);
}
#[test]
fn testRepresentativeUsesExactLetterAndPersistsSelection() {
    let mut s = companion(&[201], Rarity::Common);
    s.active.as_mut().unwrap().form = Some(forms::Form("question".into()));
    s.active.as_mut().unwrap().shiny = true;
    assert!(s.pin_appearance(201, true, Some(forms::Form("question".into()))));
    let r = roundtrip(&s);
    assert_eq!(r.representative_subject(), Some((201, true)));
    assert_eq!(
        r.representative_sprite_form(),
        Some(forms::Form("question".into()))
    );
    assert!(!s.pin_appearance(201, true, Some(forms::Form("a".into()))));
}
#[test]
fn testHatchGraduationAndReloadPreserveLetterAndShiny() {
    let mut s = companion(&[201], Rarity::Common);
    s.active.as_mut().unwrap().shiny = true;
    let form = s.active.as_ref().unwrap().form.clone();
    s.add_growth(750_000_000);
    let r = roundtrip(&s);
    assert_eq!(r.collection[0].form, form);
    assert!(r.collection[0].shiny);
}
#[test]
fn testNonUnownHatchDoesNotConsumeAnExtraRandomDraw() {
    let s = companion(&[25], Rarity::Common);
    let mut expected = 7;
    for _ in 0..3 {
        next_random(&mut expected);
    }
    assert_eq!(s.random_seed, expected);
    assert_eq!(s.active.unwrap().form, None);
}
#[test]
fn testCorruptDexEntryDroppedWhileRestSurvives() {
    let mut s = companion(&[1], Rarity::Common);
    s.add_growth(750_000_000);
    let mut value = serde_json::to_value(s).unwrap();
    value["collection"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"line":"broken"}));
    let r: Companion = serde_json::from_value(value).unwrap();
    assert_eq!(r.collection.len(), 1);
    assert_eq!(r.collection[0].species_id(), 1);
}
#[test]
fn testCorruptActiveFallsBackToEggWhileRestSurvives() {
    let mut s = companion(&[1], Rarity::Common);
    s.lifetime_tokens = 123;
    let mut value = serde_json::to_value(s).unwrap();
    value["active"]["line"] = "broken".into();
    let r: Companion = serde_json::from_value(value).unwrap();
    assert!(r.active.is_none());
    assert_eq!(r.lifetime_tokens, 123);
}
#[test]
fn testEmptyPathIDsActiveFallsBackToNilPreservingRest() {
    let mut s = companion(&[1], Rarity::Common);
    s.lifetime_tokens = 123;
    s.active.as_mut().unwrap().path.clear();
    let r = roundtrip(&s);
    assert!(r.active.is_none());
    assert_eq!(r.lifetime_tokens, 123);
}
#[test]
fn testGraduationStoresChainNames() {
    let mut s = companion(&[1, 2, 3], Rarity::Common);
    let names = s.active.as_ref().unwrap().line.names.clone();
    s.add_growth(750_000_000);
    assert_eq!(s.collection[0].line.names, names);
}
#[test]
fn testReleasingMidChainCreditsOnlyReachedForms() {
    let mut s = companion(&[1, 2, 3], Rarity::Common);
    s.lifetime_tokens = 2_000_000_000;
    s.add_growth(125_000_000);
    s.fresh_egg(None).unwrap();
    assert_eq!(s.collection[0].path, vec![1, 2]);
    assert!(!s.collection[0].graduated);
    assert!(s.collection[0].released);
}
#[test]
fn testReleasingDisguisedDittoKeepsShinyHidden() {
    let mut s = disguise();
    s.active.as_mut().unwrap().shiny = true;
    s.lifetime_tokens = 2_000_000_000;
    s.fresh_egg(None).unwrap();
    assert!(!s.collection[0].shiny);
}
#[test]
fn testRepresentativeDefaultsToCurrentCompanionForLegacySave() {
    let s = companion(&[1], Rarity::Common);
    let mut v = serde_json::to_value(&s).unwrap();
    v.as_object_mut().unwrap().remove("representative");
    let r: Companion = serde_json::from_value(v).unwrap();
    assert_eq!(r.representative_subject(), s.representative_subject());
}
#[test]
fn testRepresentativeSelectionUsesOwnedShinySpeciesAndPersists() {
    let mut s = companion(&[1, 2, 3], Rarity::Common);
    s.active.as_mut().unwrap().shiny = true;
    s.add_growth(750_000_000);
    assert!(s.pin(Some(2)));
    assert_eq!(roundtrip(&s).representative_subject(), Some((2, true)));
    assert!(!s.pin(Some(999)));
    assert_eq!(s.representative, Some(2));
}
#[test]
fn testRepresentativeSelectionSurvivesGraduation() {
    let mut s = companion(&[1, 2, 3], Rarity::Common);
    s.pin(Some(1));
    let before = s.representative_subject();
    s.add_growth(750_000_000);
    assert_eq!(s.representative_subject(), before);
}
#[test]
fn testFreshEggKeepsRepresentativeSelectionOfReleasedSpecies() {
    let mut s = companion(&[1, 2, 3], Rarity::Common);
    s.pin(Some(1));
    let before = s.representative_subject();
    s.lifetime_tokens = 2_000_000_000;
    s.fresh_egg(None).unwrap();
    assert_eq!(s.representative_subject(), before);
}
#[test]
fn testUnavailableRepresentativeSelectionIsDroppedAtLoad() {
    let mut s = companion(&[1], Rarity::Common);
    s.representative = Some(999);
    s.sanitize();
    assert_eq!(s.representative, None);
}
#[test]
fn testLocalizedName() {
    let mut l = line(&[1], Rarity::Common);
    l.names
        .get_mut(&1)
        .unwrap()
        .insert("ko".into(), "포1".into());
    assert_eq!(l.localized_name(1, "ko"), "포1");
    assert_eq!(l.localized_name(1, "de"), "D1");
    assert_eq!(l.localized_name(1, "ja"), "P1");
    assert_eq!(l.localized_name(999, "en"), "#999");
}
#[test]
fn testDexStoredChainNamesResolvePerLanguage() {
    let l = line(&[1, 2, 3], Rarity::Common);
    for id in 1..=3 {
        assert_eq!(l.localized_name(id, "en"), format!("P{id}"));
        assert_eq!(l.localized_name(id, "de"), format!("D{id}"));
    }
}
#[test]
fn testLineNodesPreviewsCompleteLinearEvolution() {
    use evolution::{LineContent::*, LineState::*};
    let s = companion(&[1, 2, 3], Rarity::Common);
    assert_eq!(
        s.line_nodes()
            .iter()
            .map(|i| (i.content.clone(), i.state))
            .collect::<Vec<_>>(),
        vec![
            (Species(1), Current),
            (Species(2), Future),
            (Species(3), Future)
        ]
    );
}
#[test]
fn testRealizedLineItemsUsesStageIndexForCurrentMarker() {
    use evolution::LineState::*;
    assert_eq!(
        evolution::realized_line_items(&[1, 2, 3], 1)
            .iter()
            .map(|i| i.state)
            .collect::<Vec<_>>(),
        vec![Done, Current, Done]
    );
}
#[test]
fn testLineNodesHidesUnresolvedWurmpleBranchAsSingleMystery() {
    use evolution::{LineContent::*, LineState::*};
    let mut s = Companion {
        egg_tokens: 5_000_000,
        ..Default::default()
    };
    let mut l = line(&[265, 266, 267], Rarity::Common);
    l.tree.children.push(node(268, vec![node(269, vec![])]));
    s.hatch(l);
    assert_eq!(
        s.line_nodes()
            .iter()
            .map(|i| (i.content.clone(), i.state))
            .collect::<Vec<_>>(),
        vec![(Species(265), Current), (Mystery, Future)]
    );
}
#[test]
fn testLineNodesShowsKnownPrefixBeforeDownstreamBranchAsMystery() {
    use evolution::{LineContent::*, LineState::*};
    let mut s = Companion {
        egg_tokens: 5_000_000,
        ..Default::default()
    };
    let mut l = line(&[60, 61, 62], Rarity::Common);
    l.tree.children[0].children.push(node(186, vec![]));
    s.hatch(l);
    assert_eq!(
        s.line_nodes()
            .iter()
            .map(|i| (i.content.clone(), i.state))
            .collect::<Vec<_>>(),
        vec![
            (Species(60), Current),
            (Species(61), Future),
            (Mystery, Future)
        ]
    );
}
#[test]
fn testLineNodesRevealsChosenWurmpleBranchAfterEvolution() {
    use evolution::{LineContent::*, LineState::*};
    let mut s = Companion {
        egg_tokens: 5_000_000,
        ..Default::default()
    };
    let mut l = line(&[265, 266, 267], Rarity::Common);
    l.tree.children.push(node(268, vec![node(269, vec![])]));
    s.hatch(l);
    s.add_growth(125_000_000);
    let path = s.active.as_ref().unwrap().path.clone();
    assert_eq!(
        s.line_nodes()
            .iter()
            .map(|i| (i.content.clone(), i.state))
            .collect::<Vec<_>>(),
        vec![
            (Species(path[0]), Done),
            (Species(path[1]), Current),
            (Species(path[2]), Future)
        ]
    );
}
#[test]
fn testStoredFormIsNormalizedAgainstTheLine() {
    let mut s = companion(&[412, 413], Rarity::Common);
    s.active.as_mut().unwrap().form = Some(forms::Form("sky".into()));
    let mut m = companion(&[1], Rarity::Common).active.unwrap();
    m.form = Some(forms::Form("sandy".into()));
    s.collection.push(m);
    s.sanitize();
    assert_eq!(s.active.unwrap().form, Some(forms::Form("plant".into())));
    assert_eq!(s.collection[0].form, None);
}
#[test]
fn testEvolutionKeepsTheCloakAndMothimKeepsItsSingleLook() {
    let mut s = companion(&[412, 413], Rarity::Common);
    s.active.as_mut().unwrap().form = Some(forms::Form("sandy".into()));
    s.add_growth(250_000_000);
    let mut m = companion(&[412, 414], Rarity::Common);
    m.active.as_mut().unwrap().form = Some(forms::Form("sandy".into()));
    m.add_growth(750_000_000);
    s.collection = m.collection;
    let sandy = forms::Form("sandy".into());
    assert!(s.owns_species(413, Some(&sandy)));
    assert!(!s.owns_species(413, Some(&forms::Form("plant".into()))));
    assert!(s.owns_species(414, None));
}
#[test]
fn testLineFormCountsOnlyOnceEverySpeciesShowingItIsOwned() {
    let sandy = forms::Form("sandy".into());
    let mut s = companion(&[412, 414], Rarity::Common);
    s.active.as_mut().unwrap().form = Some(sandy.clone());
    s.add_growth(750_000_000);
    assert!(!s.collected_line_forms(412).contains(&sandy));
    let mut worm = companion(&[412, 413], Rarity::Common);
    worm.active.as_mut().unwrap().form = Some(sandy.clone());
    worm.add_growth(750_000_000);
    s.collection.extend(worm.collection);
    assert_eq!(
        s.collected_line_forms(412),
        std::collections::BTreeSet::from([sandy])
    );
    assert!(s.collected_line_forms(201).is_empty());
}
#[test]
fn testDetailListsOwnedFormsAndMainDexKeepsOneCell() {
    let mut s = Companion::default();
    for form in ["land", "sky"] {
        let mut m = companion(&[492], Rarity::Legendary).active.unwrap();
        m.form = Some(forms::Form(form.into()));
        m.graduated = true;
        s.collection.push(m);
    }
    assert_eq!(s.form_species(492).len(), 2);
    assert_eq!(
        s.form_species(492)
            .iter()
            .map(|f| f.form.clone().unwrap().0)
            .collect::<Vec<_>>(),
        vec!["land", "sky"]
    );
}
#[test]
fn testHatchWithoutPreparationRollsAmongTheLineForms() {
    let mut seen = std::collections::BTreeSet::new();
    for seed in 0..100 {
        let mut s = Companion {
            random_seed: seed,
            egg_tokens: 5_000_000,
            ..Default::default()
        };
        s.hatch(line(&[492], Rarity::Legendary));
        seen.insert(s.active.unwrap().form.unwrap());
    }
    assert_eq!(seen, forms::forms(492).into_iter().collect());
}
#[test]
fn testRevealedDittoDropsTheDisguiseForm() {
    let mut s = companion(&[412, 413], Rarity::Common);
    s.active.as_mut().unwrap().ditto_disguise = Some(412);
    s.active.as_mut().unwrap().form = Some(forms::Form("sandy".into()));
    s.add_growth(250_000_000);
    s.reveal_ditto(line(&[132], Rarity::Rare));
    assert_eq!(s.active.unwrap().form, None);
}
#[test]
fn testCollectionIncludesLegacyActiveAndReleasedFormsButCountsShinyOnlyOnce() {
    let mut s = companion(&[201], Rarity::Common);
    s.active.as_mut().unwrap().form = Some(forms::Form("question".into()));
    s.active.as_mut().unwrap().shiny = true;
    for (form, shiny) in [
        (None, false),
        (Some("b"), false),
        (Some("b"), true),
        (Some("exclamation"), true),
    ] {
        let mut m = companion(&[201], Rarity::Common).active.unwrap();
        m.form = form.map(|f| forms::Form(f.into()));
        m.shiny = shiny;
        m.graduated = true;
        s.collection.push(m);
    }
    s.collection
        .push(companion(&[25], Rarity::Common).active.unwrap());
    let fs = s.form_species(201);
    assert_eq!(
        fs.iter()
            .map(|d| d.form.clone().unwrap().0)
            .collect::<Vec<_>>(),
        vec!["a", "b", "exclamation", "question"]
    );
    assert_eq!(
        fs.iter().map(|d| d.shiny).collect::<Vec<_>>(),
        vec![false, true, true, true]
    );
    assert_eq!(
        fs.iter().map(|d| d.raising).collect::<Vec<_>>(),
        vec![false, false, false, true]
    );
}
#[test]
fn testMissingADoesNotCreateAnOwnedFormOrPermitAnUncollectedRepresentative() {
    let mut s = companion(&[201], Rarity::Common);
    s.active.as_mut().unwrap().form = Some(forms::Form("question".into()));
    s.active.as_mut().unwrap().shiny = true;
    s.add_growth(750_000_000);
    assert_eq!(s.form_species(201).len(), 1);
    assert_eq!(
        s.form_species(201)[0].form,
        Some(forms::Form("question".into()))
    );
    assert!(!s.pin_appearance(201, true, Some(forms::Form("a".into()))));
    assert!(s.pin_appearance(201, true, Some(forms::Form("question".into()))));
}
#[test]
fn testLegacyAndInvalidFormFieldsDefaultToAWithoutLosingProgress() {
    for form in [None, Some(forms::Form("future".into()))] {
        let mut s = companion(&[201], Rarity::Common);
        s.active.as_mut().unwrap().form = form;
        s.add_growth(123);
        s.sanitize();
        assert_eq!(
            s.active.as_ref().unwrap().form,
            Some(forms::Form("a".into()))
        );
        assert_eq!(s.active.unwrap().tokens, 123);
    }
}
#[test]
fn testAppearanceOwnershipIsIndependentForEachUnownForm() {
    let mut s = companion(&[201], Rarity::Common);
    s.active.as_mut().unwrap().form = Some(forms::Form("a".into()));
    s.active.as_mut().unwrap().shiny = false;
    let mut m = companion(&[201], Rarity::Common).active.unwrap();
    m.form = Some(forms::Form("b".into()));
    m.shiny = true;
    s.collection.push(m);
    assert!(!s.pin_appearance(201, true, Some(forms::Form("a".into()))));
    assert!(!s.pin_appearance(201, false, Some(forms::Form("b".into()))));
    assert!(s.pin_appearance(201, false, Some(forms::Form("a".into()))));
    assert!(s.pin_appearance(201, true, Some(forms::Form("b".into()))));
}
#[test]
fn testFormIsOnlyAppliedToUnown() {
    let mut s = companion(&[25], Rarity::Common);
    s.active.as_mut().unwrap().form = Some(forms::Form("b".into()));
    s.sanitize();
    assert_eq!(s.active.unwrap().form, None);
}
#[test]
fn testReleasingActiveLetterPreservesFormProfileAndRepresentative() {
    let mut s = companion(&[201], Rarity::Common);
    s.active.as_mut().unwrap().form = Some(forms::Form("question".into()));
    s.active.as_mut().unwrap().shiny = true;
    let profile = s.active.as_ref().unwrap().profile.clone();
    s.pin_appearance(201, true, Some(forms::Form("question".into())));
    s.lifetime_tokens = 2_000_000_000;
    s.fresh_egg(None).unwrap();
    assert_eq!(s.collection[0].form, Some(forms::Form("question".into())));
    assert_eq!(s.collection[0].profile, profile);
    assert_eq!(s.representative_subject(), Some((201, true)));
    assert_eq!(
        s.representative_sprite_form(),
        Some(forms::Form("question".into()))
    );
}
#[test]
fn testDifferentLettersKeepSpeciesBasedRepeatGrowthAtEveryDifficulty() {
    for difficulty in [0.1, 1., 2.] {
        let mut s = companion(&[201], Rarity::Common);
        s.set_growth_difficulty(difficulty);
        s.active.as_mut().unwrap().form = Some(forms::Form("a".into()));
        s.add_growth(s.progress().1);
        s.egg_tokens = s.progress().1;
        s.hatch(line(&[201], Rarity::Common));
        assert!(s.active.as_ref().unwrap().growth_boost);
        assert_eq!(s.progress().1, Balance::scaled(375_000_000, difficulty));
    }
}
#[test]
fn testFormOwnershipDoesNotChangeSpeciesSelectionOrShinyRolls() {
    let a = companion(&[201], Rarity::Common);
    let mut b = Companion {
        random_seed: 7,
        egg_tokens: 5_000_000,
        ..Default::default()
    };
    let mut m = a.active.clone().unwrap();
    m.graduated = true;
    b.collection.push(m);
    b.hatch(line(&[201], Rarity::Common));
    assert_eq!(
        a.active.as_ref().unwrap().shiny,
        b.active.as_ref().unwrap().shiny
    );
    assert_eq!(
        a.active.as_ref().unwrap().nature,
        b.active.as_ref().unwrap().nature
    );
    assert_eq!(
        a.active.unwrap().line.base_id,
        b.active.unwrap().line.base_id
    );
}
#[test]
fn testRevealRebasesDisguiseProfileAndLoadsDittoDetails() {
    let mut s = disguise();
    s.enrich_profiles(&profile::Details {
        species_id: 1,
        types: vec![],
        gender_rate: 1,
        base_stats: Default::default(),
        abilities: vec![profile::Ability {
            name: "overgrow".into(),
            slot: 1,
            is_hidden: false,
        }],
        moves: vec![profile::Move {
            name: "tackle".into(),
            learn_methods: vec![profile::LearnMethod {
                method: "level-up".into(),
                level: 1,
            }],
        }],
    });
    s.add_growth(300_000_000);
    s.reveal_ditto(line(&[132], Rarity::Rare));
    s.enrich_profiles(&profile::Details {
        species_id: 132,
        types: vec![],
        gender_rate: -1,
        base_stats: Default::default(),
        abilities: vec![profile::Ability {
            name: "limber".into(),
            slot: 1,
            is_hidden: false,
        }],
        moves: vec![profile::Move {
            name: "transform".into(),
            learn_methods: vec![profile::LearnMethod {
                method: "level-up".into(),
                level: 1,
            }],
        }],
    });
    let p = s.active.unwrap().profile.unwrap();
    assert_eq!(p.gender, Some(profile::Gender::Genderless));
    assert_eq!(p.ability_name, Some("limber".into()));
    assert_eq!(
        p.moves.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
        vec!["transform"]
    );
    assert_eq!(p.growth_tokens, 500_000_000);
    assert_eq!(p.level, 20);
}
#[test]
fn testHatchUsesThePreparedFormOfAnyLine() {
    let mut s = Companion {
        random_seed: 7,
        ..Default::default()
    };
    assert!(s.prepare_egg(line(&[492], Rarity::Legendary)));
    let form = s.pending_form.clone();
    s.egg_tokens = 5_000_000;
    s.hatch_prepared();
    assert_eq!(s.active.unwrap().form, form);
    assert!(s.pending_line.is_none());
    assert!(s.pending_form.is_none());
}
#[test]
fn testEggPrefetchStoresPendingAndHatchUsesIt() {
    let mut s = Companion::default();
    assert!(s.prepare_egg(line(&[1, 2, 3], Rarity::Common)));
    let mut restored = roundtrip(&s);
    assert_eq!(restored.pending_line.as_ref().unwrap().base_id, 1);
    restored.egg_tokens = 5_000_000;
    restored.hatch_prepared();
    assert_eq!(restored.active.unwrap().line.base_id, 1);
    assert!(restored.pending_line.is_none());
}
#[test]
fn testPrefetchPersistsLetterAndHatchConsumesItAfterRNGChanges() {
    let mut s = Companion {
        random_seed: 7,
        ..Default::default()
    };
    s.prepare_egg(line(&[201], Rarity::Common));
    let form = s.pending_form.clone();
    let mut r = roundtrip(&s);
    r.random_seed = 9999;
    r.egg_tokens = 5_000_000;
    r.hatch_prepared();
    assert_eq!(r.active.unwrap().form, form);
    assert_eq!(r.pending_form, None);
}
#[test]
fn testLegacyPendingUnownKeepsDefaultAAtHatch() {
    let mut s = Companion {
        pending_line: Some(line(&[201], Rarity::Common)),
        egg_tokens: 5_000_000,
        ..Default::default()
    };
    s.hatch_prepared();
    assert_eq!(s.active.unwrap().form, Some(forms::Form("a".into())));
}
#[test]
fn testSanitizationClearsPendingLetterWithoutMatchingEgg() {
    let mut s = Companion {
        pending_form: Some(forms::Form("question".into())),
        ..Default::default()
    };
    s.sanitize();
    assert_eq!(s.pending_form, None);
    s.pending_line = Some(line(&[25], Rarity::Common));
    s.pending_form = Some(forms::Form("question".into()));
    s.sanitize();
    assert_eq!(s.pending_form, None);
    s = companion(&[25], Rarity::Common);
    s.pending_line = Some(line(&[201], Rarity::Common));
    s.pending_form = Some(forms::Form("question".into()));
    s.sanitize();
    assert!(s.pending_line.is_none());
    assert!(s.pending_form.is_none());
}
#[test]
fn testSaveExportAndImportPreservePendingEggLetter() {
    let mut s = Companion {
        random_seed: 7,
        ..Default::default()
    };
    s.prepare_egg(line(&[201], Rarity::Common));
    let r = storage::decode(&storage::encode(&s, 42).unwrap())
        .unwrap()
        .state;
    assert_eq!(r.pending_form, s.pending_form);
    assert_eq!(r.pending_line.as_ref().unwrap().base_id, 201);
}
#[test]
fn testSaveExportAndImportPreserveActiveDexAndRepresentativeForms() {
    let mut s = companion(&[201], Rarity::Common);
    s.active.as_mut().unwrap().form = Some(forms::Form("question".into()));
    s.active.as_mut().unwrap().shiny = true;
    s.pin_appearance(201, true, Some(forms::Form("question".into())));
    let mut archived = s.active.clone().unwrap();
    archived.form = Some(forms::Form("b".into()));
    archived.graduated = true;
    s.collection.push(archived);
    let r = storage::decode(&storage::encode(&s, 42).unwrap())
        .unwrap()
        .state;
    assert_eq!(r.active.unwrap().form, Some(forms::Form("question".into())));
    assert_eq!(r.collection[0].form, Some(forms::Form("b".into())));
    assert_eq!(r.representative_form, Some(forms::Form("question".into())));
}
#[test]
fn testReloadPreservesCompleteShortPlannedRouteLength() {
    let mut s = companion(&[1, 4], Rarity::Common);
    s.active
        .as_mut()
        .unwrap()
        .line
        .tree
        .children
        .push(node(2, vec![node(3, vec![])]));
    let mut r = roundtrip(&s);
    r.sanitize();
    assert_eq!(r.active.as_ref().unwrap().path, vec![1, 4]);
    assert_eq!(r.progress().1, 250_000_000);
}
#[test]
fn testReloadLegacyIncompletePlanMigratesToPersistedCompleteRoute() {
    let mut s = companion(&[1, 2, 3], Rarity::Common);
    s.active.as_mut().unwrap().path = vec![1];
    s.sanitize();
    assert_eq!(s.active.as_ref().unwrap().path, vec![1, 2, 3]);
    assert_eq!(s.active.unwrap().stage, 0);
}
#[test]
fn testReloadRepairsInvalidPlanSuffixWithoutRewindingRealizedPath() {
    let mut s = companion(&[1, 2, 3], Rarity::Common);
    s.active.as_mut().unwrap().path = vec![1, 2, 999];
    s.active.as_mut().unwrap().stage = 1;
    s.sanitize();
    assert_eq!(s.active.as_ref().unwrap().path, vec![1, 2, 3]);
    assert_eq!(s.active.unwrap().stage, 1);
}
#[test]
fn testReloadWrongRootNormalizesPathWithoutChangingIdentityOrDisguise() {
    let mut s = disguise();
    s.active.as_mut().unwrap().path = vec![999];
    s.active.as_mut().unwrap().tokens = 42;
    s.active.as_mut().unwrap().shiny = true;
    s.active.as_mut().unwrap().nature = "Timid".into();
    s.sanitize();
    let m = s.active.unwrap();
    assert_eq!(m.path, vec![1, 2, 3]);
    assert_eq!(m.tokens, 42);
    assert!(m.shiny);
    assert_eq!(m.nature, "Timid");
    assert_eq!(m.ditto_disguise, Some(1));
}
#[test]
fn testReloadLeafCurrentPlanDoesNotConsumeRNG() {
    let mut s = companion(&[1], Rarity::Common);
    let seed = s.random_seed;
    s.sanitize();
    assert_eq!(s.random_seed, seed);
    assert_eq!(s.active.unwrap().path, vec![1]);
}
#[test]
fn testCandyPickerMatchesApparentPokemonButOnlySpendsEnoughToReveal() {
    let mut s = disguise();
    let mut normal = companion(&[1, 2, 3], Rarity::Common);
    for model in [&mut s, &mut normal] {
        model.add_growth(100_000_000);
        model.inventory.candies = 5;
    }
    s.use_candy(5).unwrap();
    assert_eq!(s.inventory.candies, 4);
    assert!(s.use_candy(5).is_err());
    s.reveal_ditto(line(&[132], Rarity::Rare));
    assert_eq!(s.active.as_ref().unwrap().tokens, 75_000_000);
    s.use_candy(2).unwrap();
    assert_eq!(s.active.unwrap().tokens, 275_000_000);
    assert_eq!(s.inventory.candies, 2);
}
#[test]
fn testDittoExcludedFromRestFallback() {
    assert!(
        PokeApi::new(std::path::PathBuf::from("/not/a/cache"))
            .base_species(132)
            .unwrap()
            .is_none()
    );
}
#[test]
fn testPrefetchUsesOwnedFormWeights() {
    let mut s = Companion {
        random_seed: 7,
        ..Default::default()
    };
    let mut mon = companion(&[201], Rarity::Common).active.unwrap();
    mon.form = Some(forms::Form("a".into()));
    mon.graduated = true;
    s.collection.push(mon);
    let mut seed = 7;
    let expected = forms::roll(
        next_random(&mut seed),
        &forms::forms(201),
        &std::collections::BTreeSet::from([forms::Form("a".into())]),
    );
    s.prepare_egg(line(&[201], Rarity::Common));
    assert_eq!(s.pending_form, expected);
}
#[test]
fn testHatchUsesOwnedFormWeightsAfterUnchangedShinyNatureAndProfileRolls() {
    let mut s = Companion {
        random_seed: 7,
        egg_tokens: 5_000_000,
        ..Default::default()
    };
    let mut mon = companion(&[201], Rarity::Common).active.unwrap();
    mon.form = Some(forms::Form("a".into()));
    mon.graduated = true;
    s.collection.push(mon);
    let mut seed = 7;
    let shiny = Companion::rolls_shiny(next_random(&mut seed), false);
    let nature = NATURES[(next_random(&mut seed) % 25) as usize];
    let profile_seed = next_random(&mut seed);
    let form = forms::roll(
        next_random(&mut seed),
        &forms::forms(201),
        &std::collections::BTreeSet::from([forms::Form("a".into())]),
    );
    s.hatch(line(&[201], Rarity::Common));
    let mon = s.active.unwrap();
    assert_eq!(mon.shiny, shiny);
    assert_eq!(mon.nature, nature);
    assert_eq!(mon.profile.unwrap().seed, profile_seed);
    assert_eq!(mon.form, form);
}
