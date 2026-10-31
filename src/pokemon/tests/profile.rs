use super::super::{Rarity, profile::*};

fn details() -> Details {
    Details {
        species_id: 79,
        types: vec![],
        gender_rate: 8,
        base_stats: [
            ("hp", 90),
            ("attack", 65),
            ("defense", 65),
            ("special-attack", 40),
            ("special-defense", 40),
            ("speed", 15),
        ]
        .into_iter()
        .map(|(name, value)| (name.into(), value))
        .collect(),
        abilities: vec![ability("oblivious", 1, false)],
        moves: [
            ("tackle", 1),
            ("curse", 1),
            ("yawn", 1),
            ("growl", 5),
            ("water-gun", 9),
        ]
        .into_iter()
        .map(|(name, level)| Move {
            name: name.into(),
            learn_methods: vec![LearnMethod {
                method: "level-up".into(),
                level,
            }],
        })
        .chain([Move {
            name: "surf".into(),
            learn_methods: vec![LearnMethod {
                method: "machine".into(),
                level: 0,
            }],
        }])
        .collect(),
    }
}
fn ability(name: &str, slot: i32, is_hidden: bool) -> Ability {
    Ability {
        name: name.into(),
        slot,
        is_hidden,
    }
}
fn minimal_details(gender_rate: i32, abilities: Vec<Ability>) -> Details {
    Details {
        species_id: 999,
        types: vec![],
        gender_rate,
        base_stats: [("hp".into(), 50)].into(),
        abilities,
        moves: vec![],
    }
}

// upstream: PokemonProfileTests.swift::testGenerationIsDeterministicAndIVsStayInRange
#[test]
fn testGenerationIsDeterministicAndIVsStayInRange() {
    let a = Profile::generate(42, "same".into());
    assert_eq!(a, Profile::generate(42, "same".into()));
    assert!(a.ivs.iter().all(|iv| (0..=31).contains(iv)));
}

// upstream: PokemonProfileTests.swift::testEnrichmentRollsPersistentIdentityAndFourLegalMoves
#[test]
fn testEnrichmentRollsPersistentIdentityAndFourLegalMoves() {
    let mut p = Profile::generate(7, "p".into());
    p.level = 9;
    p.enrich(&details());
    assert_eq!(p.gender, Some(Gender::Female));
    assert_eq!(p.ability_name.as_deref(), Some("oblivious"));
    assert_eq!(p.ability_slot, Some(1));
    assert_eq!(p.moves.len(), 4);
    assert!(p.moves.iter().any(|m| m.name == "water-gun"));
    assert!(!p.moves.iter().any(|m| m.name == "surf"));
    let first = p.clone();
    p.enrich(&details());
    assert_eq!(p, first);
}

// upstream: PokemonProfileTests.swift::testNatureChangesCalculatedNonHPStat
#[test]
fn testNatureChangesCalculatedNonHPStat() {
    let mut p = Profile::generate(1, "neutral".into());
    p.level = 50;
    let stats = computed_stats(&details(), &p, Some("adamant"));
    assert_eq!(
        stats["attack"],
        ((((2 * 65 + p.ivs[1]) * 50) / 100 + 5) as f64 * 1.1).floor() as i32
    );
    assert_eq!(
        stats["special-attack"],
        ((((2 * 40 + p.ivs[3]) * 50) / 100 + 5) as f64 * 0.9).floor() as i32
    );
}

// upstream: PokemonProfileTests.swift::testShedinjaAlwaysHasOneHP
#[test]
fn testShedinjaAlwaysHasOneHP() {
    let mut d = minimal_details(-1, vec![]);
    d.species_id = 292;
    d.base_stats = [("hp".into(), 1), ("attack".into(), 90)].into();
    for level in [1, 50, 100] {
        let mut p = Profile::generate(3, "shedinja".into());
        p.level = level;
        let stats = computed_stats(&d, &p, Some("hardy"));
        assert_eq!(stats["hp"], 1);
        assert_eq!(stats["attack"], ((2 * 90 + p.ivs[1]) * level) / 100 + 5);
    }
    d.species_id = 999;
    let mut p = Profile::generate(3, "base-1".into());
    p.level = 50;
    assert_eq!(
        computed_stats(&d, &p, None)["hp"],
        ((2 + p.ivs[0]) * 50) / 100 + 60
    );
}

// upstream: PokemonProfileTests.swift::testActualStatScaleExpandsForHighHPPokemon
#[test]
fn testActualStatScaleExpandsForHighHPPokemon() {
    assert_eq!(display_scale_maximum(&[180, 299]), 300);
    assert_eq!(display_scale_maximum(&[651, 300]), 700);
}

// upstream: PokemonProfileTests.swift::testEveryNatureChangesExactlyItsExpectedNumberOfStats
#[test]
fn testEveryNatureChangesExactlyItsExpectedNumberOfStats() {
    assert_eq!(NATURES.len(), 25);
    for nature in NATURES {
        let raised: Vec<_> = STAT_ORDER
            .into_iter()
            .filter(|s| nature_modifier(nature, s) == 1.1)
            .collect();
        let lowered: Vec<_> = STAT_ORDER
            .into_iter()
            .filter(|s| nature_modifier(nature, s) == 0.9)
            .collect();
        if ["hardy", "docile", "serious", "bashful", "quirky"].contains(&nature) {
            assert!(raised.is_empty() && lowered.is_empty());
        } else {
            assert_eq!(raised.len(), 1, "{nature}");
            assert_eq!(lowered.len(), 1, "{nature}");
            assert_ne!(raised, lowered);
        }
    }
}

// upstream: PokemonProfileTests.swift::testGrowthMapsHatchToFiveAndGraduationToHundred
#[test]
fn testGrowthMapsHatchToFiveAndGraduationToHundred() {
    let mut p = Profile::generate(1, "growth".into());
    assert_eq!(p.level, 5);
    p.apply_growth(750_000_000, Rarity::Common);
    assert_eq!(p.level, 100);
}

// upstream: PokemonProfileTests.swift::testGenderlessAndMaleBranchesAreResolved
#[test]
fn testGenderlessAndMaleBranchesAreResolved() {
    let mut p = Profile::generate(1, "genderless".into());
    p.enrich(&minimal_details(-1, vec![]));
    assert_eq!(p.gender, Some(Gender::Genderless));
    let mut p = Profile::generate(1, "male".into());
    p.enrich(&minimal_details(0, vec![]));
    assert_eq!(p.gender, Some(Gender::Male));
}

// upstream: PokemonProfileTests.swift::testHiddenAbilityRollCanSelectAHiddenSlot
#[test]
fn testHiddenAbilityRollCanSelectAHiddenSlot() {
    let mut p = Profile::generate(115, "hidden-roll".into());
    p.enrich(&minimal_details(
        -1,
        vec![
            ability("hidden-one", 3, true),
            ability("hidden-two", 4, true),
        ],
    ));
    assert_eq!(p.ability_name.as_deref(), Some("hidden-two"));
    assert_eq!(p.ability_slot, Some(4));
    assert!(p.ability_is_hidden);
}

// upstream: PokemonProfileTests.swift::testAllHiddenAbilitiesFallBackWhenRareRollMisses
#[test]
fn testAllHiddenAbilitiesFallBackWhenRareRollMisses() {
    let mut p = Profile::generate(1, "hidden-fallback".into());
    p.enrich(&minimal_details(
        -1,
        vec![
            ability("first-hidden", 2, true),
            ability("second-hidden", 3, true),
        ],
    ));
    assert_eq!(p.ability_name.as_deref(), Some("first-hidden"));
    assert_eq!(p.ability_slot, Some(2));
    assert!(p.ability_is_hidden);
}

// upstream: PokemonProfileTests.swift::testAbilityNameFallbackChainRepairsPersistedSelections
#[test]
fn testAbilityNameFallbackChainRepairsPersistedSelections() {
    let normal = ability("normal", 1, false);
    let hidden = ability("hidden", 3, true);
    let mut p = Profile::generate(2, "slot-only".into());
    p.ability_slot = Some(1);
    p.ability_is_hidden = true;
    p.enrich(&minimal_details(-1, vec![normal.clone()]));
    assert_eq!(p.ability_name.as_deref(), Some("normal"));
    assert!(!p.ability_is_hidden);
    let mut p = Profile::generate(3, "normal-fallback".into());
    p.ability_slot = Some(99);
    p.ability_is_hidden = true;
    p.enrich(&minimal_details(-1, vec![hidden.clone(), normal]));
    assert_eq!(p.ability_name.as_deref(), Some("normal"));
    assert!(!p.ability_is_hidden);
    let mut p = Profile::generate(4, "hidden-fallback-chain".into());
    p.ability_slot = Some(99);
    p.enrich(&minimal_details(-1, vec![hidden]));
    assert_eq!(p.ability_name.as_deref(), Some("hidden"));
    assert!(p.ability_is_hidden);
}

// upstream: PokemonProfileTests.swift::testSpeciesIdentityRebaseKeepsIVsButClearsDisguiseMetadata
#[test]
fn testSpeciesIdentityRebaseKeepsIVsButClearsDisguiseMetadata() {
    let mut p = Profile::generate(7, "ditto".into());
    p.enrich(&details());
    let ivs = p.ivs;
    p.apply_growth(125_000_000, Rarity::Common);
    p.rebase_species(Rarity::Common, Rarity::Rare);
    assert_eq!(p.instance_id, "ditto");
    assert_eq!(p.ivs, ivs);
    assert_eq!(p.gender, None);
    assert_eq!(p.ability_slot, None);
    assert_eq!(p.ability_name, None);
    assert!(!p.ability_is_hidden);
    assert!(p.moves.is_empty());
    assert_eq!(p.growth_tokens, 500_000_000);
    assert_eq!(p.level, 20);
}

#[test]
fn hostile_profile_sanitization_bounds_untrusted_values() {
    let mut p = Profile::generate(5, String::new());
    p.level = 9_999;
    p.growth_tokens = u64::MAX;
    p.ivs = [999, -5, 31, 64, -1, 500];
    p.moves = (0..400)
        .map(|i| KnownMove {
            name: format!("move-{i}-{}", "x".repeat(200)),
            learned_at_level: 900 + i,
        })
        .collect();
    p.sanitize();
    assert_eq!(p.level, 100);
    assert_eq!(p.growth_tokens, MAX_TOKENS);
    assert_eq!(p.ivs, [31, 0, 31, 31, 0, 31]);
    assert_eq!(p.moves.len(), 4);
    assert!(
        p.moves
            .iter()
            .all(|m| m.name.chars().count() <= 80 && (0..=100).contains(&m.learned_at_level))
    );
    assert!(!p.instance_id.is_empty());
    p.level = -99;
    p.sanitize();
    assert_eq!(p.level, 5);
}

fn hostile() -> Profile {
    let mut p = Profile::generate(5, String::new());
    p.level = 9999;
    p.growth_tokens = u64::MAX;
    p.ivs = [999, -5, 31, 64, -1, 500];
    p.moves = (0..400)
        .map(|i| KnownMove {
            name: format!("move-{i}-{}", "x".repeat(200)),
            learned_at_level: 900 + i,
        })
        .collect();
    p
}
fn assert_clamped(p: &Profile) {
    assert_eq!(p.level, 100);
    assert_eq!(p.growth_tokens, MAX_TOKENS);
    assert_eq!(p.ivs, [31, 0, 31, 31, 0, 31]);
    assert_eq!(p.moves.len(), 4);
    assert!(
        p.moves
            .iter()
            .all(|m| m.name.chars().count() <= 80 && (0..=100).contains(&m.learned_at_level))
    );
    assert!(!p.instance_id.is_empty());
}
// upstream: PokemonProfileTests.swift::testImportClampsHostileActiveProfile
#[test]
fn testImportClampsHostileActiveProfile() {
    let mut s = super::economy::hatched();
    s.active.as_mut().unwrap().profile = Some(hostile());
    s.sanitize();
    assert_clamped(s.active.as_ref().unwrap().profile.as_ref().unwrap());
}
// upstream: PokemonProfileTests.swift::testImportClampsHostileDexProfile
#[test]
fn testImportClampsHostileDexProfile() {
    let mut s = super::economy::hatched();
    let mut mon = s.active.take().unwrap();
    mon.profile = Some(hostile());
    s.collection.push(mon);
    s.sanitize();
    assert_clamped(s.collection[0].profile.as_ref().unwrap());
}
// upstream: PokemonProfileTests.swift::testImportRaisesProfileLevelToHatchMinimum
#[test]
fn testImportRaisesProfileLevelToHatchMinimum() {
    let mut s = super::economy::hatched();
    s.active.as_mut().unwrap().profile.as_mut().unwrap().level = -99;
    s.sanitize();
    assert_eq!(s.active.unwrap().profile.unwrap().level, 5);
}
// upstream: PokemonProfileTests.swift::testParserKeepsOnlySupportedVersionGroupBeforeCaching
#[test]
fn testParserKeepsOnlySupportedVersionGroupBeforeCaching() {
    use serde_json::json;
    let stats: Vec<_> = STAT_ORDER
        .iter()
        .map(|name| json!({"stat":{"name":name},"base_stat":50}))
        .collect();
    let supported = json!({"level_learned_at":5,"move_learn_method":{"name":"level-up"},"version_group":{"name":"black-2-white-2"}});
    let unsupported = json!({"level_learned_at":9,"move_learn_method":{"name":"level-up"},"version_group":{"name":"scarlet-violet"}});
    let data = json!({"stats":stats,"moves":[{"move":{"name":"mixed"},"version_group_details":[supported,unsupported.clone()]},{"move":{"name":"future-only"},"version_group_details":[unsupported]}]});
    let parsed = super::super::api::parse_details(1, &json!({"gender_rate":-1}), &data).unwrap();
    assert_eq!(parsed.moves.len(), 1);
    assert_eq!(parsed.moves[0].name, "mixed");
    assert_eq!(
        parsed.moves[0].learn_methods,
        [LearnMethod {
            method: "level-up".into(),
            level: 5
        }]
    );
}
// upstream: PokemonProfileTests.swift::testCorruptProfileRegeneratesWithoutDroppingPokemon
#[test]
fn testCorruptProfileRegeneratesWithoutDroppingPokemon() {
    let mut original = super::economy::hatched();
    original.collection.push(original.active.clone().unwrap());
    let mut json = serde_json::to_value(&original).unwrap();
    json["active"]["profile"] = serde_json::json!("broken");
    json["collection"][0]["profile"] = serde_json::json!("broken");
    let mut restored: super::super::Companion = serde_json::from_value(json).unwrap();
    restored.sanitize();
    assert_eq!(restored.active.as_ref().unwrap().species_id(), 1);
    assert!(restored.active.as_ref().unwrap().profile.is_some());
    assert_eq!(restored.collection.len(), 1);
    assert!(restored.collection[0].profile.is_some());
}
// Derived from the individual ordering portion of PokemonProfileTests.swift.
// Synthetic GPUI row identity is tested separately before mapping the full upstream case.
#[test]
fn testIndividualsReturnActiveThenNewestStoredProfiles() {
    let mut s = super::economy::hatched();
    s.active
        .as_mut()
        .unwrap()
        .profile
        .as_mut()
        .unwrap()
        .instance_id = "active-instance".into();
    let mut old = s.active.clone().unwrap();
    old.caught_at = 10;
    old.profile.as_mut().unwrap().instance_id = "stored-old".into();
    let mut new = old.clone();
    new.caught_at = 20;
    new.profile.as_mut().unwrap().instance_id = "stored-new".into();
    s.collection = vec![old, new];
    let ids: Vec<_> = s
        .individuals(1)
        .iter()
        .map(|m| m.profile.as_ref().unwrap().instance_id.as_str())
        .collect();
    assert_eq!(ids, ["active-instance", "stored-new", "stored-old"]);
    assert!(s.individuals(99).is_empty());
}
// upstream: PokemonProfileTests.swift::testDetailLoadEnrichesEveryMatchingStoredIndividual
#[test]
fn testDetailLoadEnrichesEveryMatchingStoredIndividual() {
    let mut s = super::economy::hatched();
    let active = s.active.take().unwrap();
    s.collection = vec![active.clone(), active];
    let mut d = details();
    d.species_id = 1;
    s.enrich_profiles(&d);
    assert!(
        s.collection
            .iter()
            .all(|m| m.profile.as_ref().unwrap().gender == Some(Gender::Female))
    );
    assert!(
        s.collection
            .iter()
            .all(|m| m.profile.as_ref().unwrap().ability_name.as_deref() == Some("oblivious"))
    );
    assert!(
        s.collection
            .iter()
            .all(|m| !m.profile.as_ref().unwrap().moves.is_empty())
    );
    let restored: super::super::Companion =
        serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
    assert!(
        restored
            .collection
            .iter()
            .all(|m| m.profile.as_ref().unwrap().gender == Some(Gender::Female))
    );
}
// upstream: PokemonProfileTests.swift::testReleasedLegacyEntryUsesReachedChainAsGrowthUpperBound
#[test]
fn testReleasedLegacyEntryUsesReachedChainAsGrowthUpperBound() {
    let mut s = super::economy::hatched();
    let mut released = s.active.take().unwrap();
    released.stage = 1;
    released.path.truncate(2);
    released.tokens = 0;
    released.profile = None;
    released.released = true;
    s.collection.push(released);
    s.sanitize();
    let p = s.collection[0].profile.as_ref().unwrap();
    assert_eq!(p.growth_tokens, 250_000_000);
    assert!(p.growth_tokens > 125_000_000);
    assert!(p.level > 5 && p.level < 100);
}
