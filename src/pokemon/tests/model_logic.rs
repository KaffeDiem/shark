//! Pure model cases translated from the pinned upstream XCTest suite.
use super::super::*;

fn node(id: u16, children: Vec<EvoNode>) -> EvoNode {
    EvoNode {
        species_id: id,
        children,
    }
}
fn tree() -> EvoNode {
    node(1, vec![node(2, vec![node(3, vec![])]), node(4, vec![])])
}
fn names() -> EvoLine {
    EvoLine {
        base_id: 1,
        tree: node(1, vec![]),
        rarity: Rarity::Common,
        names: BTreeMap::from([
            (
                1,
                BTreeMap::from([
                    ("ja-Hrkt".into(), "ピカ".into()),
                    ("ja".into(), "ピカチュウ".into()),
                    ("en".into(), "Pika".into()),
                    ("ko".into(), "피카".into()),
                ]),
            ),
            (2, BTreeMap::from([("en".into(), "Eevee".into())])),
            (3, BTreeMap::new()),
        ]),
    }
}
// upstream: ModelLogicTests.swift::testPicksLanguageSpecificThenFallsBackToEnglishThenID
#[test]
fn name_fallback_order() {
    let line = names();
    for (id, lang, expected) in [
        (1, "ja", "ピカ"),
        (1, "ko", "피카"),
        (1, "en", "Pika"),
        (2, "ja", "Eevee"),
        (2, "ko", "Eevee"),
        (3, "ko", "#3"),
        (99, "en", "#99"),
    ] {
        assert_eq!(line.localized_name(id, lang), expected);
    }
}
// upstream: ModelLogicTests.swift::testJaFallsBackFromHrktToPlainJa
#[test]
fn japanese_plain_fallback() {
    let mut line = names();
    line.names.get_mut(&1).unwrap().remove("ja-Hrkt");
    assert_eq!(line.localized_name(1, "ja"), "ピカチュウ");
}
// upstream: ModelLogicTests.swift::testGermanNameUsesGermanThenFallsBackToEnglish
#[test]
fn german_name_fallback() {
    let mut line = names();
    line.names
        .get_mut(&1)
        .unwrap()
        .insert("de".into(), "Bisasam".into());
    assert_eq!(line.localized_name(1, "de"), "Bisasam");
    assert_eq!(line.localized_name(2, "de"), "Eevee");
}
// upstream: ModelLogicTests.swift::testRussianNameUsesRussianThenFallsBackToEnglish
#[test]
fn russian_name_fallback() {
    let mut line = names();
    line.names
        .get_mut(&1)
        .unwrap()
        .insert("ru".into(), "Бульбазавр".into());
    line.names
        .get_mut(&2)
        .unwrap()
        .insert("de".into(), "Evoli".into());
    assert_eq!(line.localized_name(1, "ru"), "Бульбазавр");
    assert_eq!(line.localized_name(2, "ru"), "Eevee");
}
// upstream: ModelLogicTests.swift::testKeepsOnlyFormsWithAnimatedAssets
#[test]
fn unsupported_evolutions_pruned() {
    let tree = node(56, vec![node(57, vec![node(979, vec![])])])
        .keeping_animated_sprites()
        .unwrap();
    assert_eq!(tree.depth(), 2);
    assert_eq!(tree.final_ids(), [57]);
    assert!(tree.node(979).is_none());
}
// upstream: ModelLogicTests.swift::testDepthIsLongestPath
#[test]
fn tree_depth() {
    assert_eq!(tree().depth(), 3);
    assert_eq!(node(20, vec![]).depth(), 1);
}
// upstream: ModelLogicTests.swift::testNodeLookupByID
#[test]
fn tree_lookup() {
    let t = tree();
    assert_eq!(t.node(3).unwrap().species_id, 3);
    assert_eq!(t.node(4).unwrap().species_id, 4);
    assert!(t.node(99).is_none());
}
// upstream: ModelLogicTests.swift::testFinalIDsAreLeaves
#[test]
fn final_species_are_leaves() {
    assert_eq!(tree().final_ids(), [3, 4]);
    assert_eq!(node(20, vec![]).final_ids(), [20]);
}
// upstream: ModelLogicTests.swift::testParsesMicroMilliAndPlainSeconds
#[test]
fn iso_epoch_precisions() {
    for text in [
        "2026-06-10T11:10:00.034464+00:00",
        "2026-06-10T11:10:00.303Z",
        "2026-06-10T11:10:00Z",
    ] {
        assert!(limits::parse_epoch(text).is_some(), "{text}");
    }
}
// upstream: ModelLogicTests.swift::testReturnsNilForGarbage
#[test]
fn invalid_epoch() {
    for text in ["not-a-date", ""] {
        assert_eq!(limits::parse_epoch(text), None);
    }
}
// upstream: ModelLogicTests.swift::testMicroAndMilliResolveToSameInstant
#[test]
fn equivalent_epoch_precision() {
    assert_eq!(
        limits::parse_epoch("2026-06-10T11:10:00.000000Z"),
        limits::parse_epoch("2026-06-10T11:10:00Z")
    );
}
// upstream: ModelLogicTests.swift::testCurrentIDClampsToPath
#[test]
fn current_species_clamps_path() {
    let mut mon = super::economy::hatched().active.unwrap();
    mon.stage = 1;
    assert_eq!(mon.species_id(), 2);
    mon.path = vec![1];
    mon.stage = 5;
    assert_eq!(mon.species_id(), 1);
}
// Rust performs path-dependent repair at the Companion trust boundary via sanitize().
// upstream: ModelLogicTests.swift::testMonStateDecodeClampsStageIndexToRealizedPathBounds
#[test]
fn loaded_stage_clamped_at_trust_boundary() {
    let state = super::economy::hatched();
    for (stage, expected) in [(serde_json::json!(5), 2), (serde_json::json!(-1), 0)] {
        let mut json = serde_json::to_value(&state).unwrap();
        json["active"]["stage"] = stage;
        let mut result: Companion = serde_json::from_value(json).unwrap();
        result.sanitize();
        assert_eq!(result.active.unwrap().stage, expected);
    }
}
// Rust stores the complete plan with stage as its realized-prefix boundary.
// upstream: ModelLogicTests.swift::testMonStateRoundTripPreservesDistinctPlannedPath
#[test]
fn planned_path_round_trip_keeps_realized_prefix() {
    let state = super::economy::hatched();
    let mon = state.active.unwrap();
    let restored: Mon = serde_json::from_value(serde_json::to_value(&mon).unwrap()).unwrap();
    assert_eq!(restored.path, [1, 2, 3]);
    assert_eq!(restored.reached_species().collect::<Vec<_>>(), [1]);
}
// Rust required nature strings use Hardy as the legacy empty-identity default.
// upstream: CompanionTests.swift::testBackwardCompatibleDecode
#[test]
fn legacy_identity_defaults_preserve_active_and_collection() {
    let mut state = super::economy::hatched();
    state.collection.push(state.active.clone().unwrap());
    let mut json = serde_json::to_value(state).unwrap();
    let active = json["active"].as_object_mut().unwrap();
    active.remove("shiny");
    active.remove("nature");
    let archived = json["collection"][0].as_object_mut().unwrap();
    archived.remove("shiny");
    archived.remove("nature");
    let mut restored: Companion = serde_json::from_value(json).unwrap();
    restored.sanitize();
    assert!(!restored.active.as_ref().unwrap().shiny);
    assert_eq!(restored.active.as_ref().unwrap().nature, "Hardy");
    assert_eq!(restored.collection.len(), 1);
    assert!(!restored.collection[0].shiny);
    let round: Companion =
        serde_json::from_value(serde_json::to_value(&restored).unwrap()).unwrap();
    assert_eq!(round.active.unwrap().nature, "Hardy");
}

// upstream: ModelLogicTests.swift::testCompanionStateEncodeDecodeRoundTrip
#[test]
fn companion_state_round_trip() {
    let mut state = super::economy::hatched();
    state.lifetime_tokens = 42;
    state.egg_tokens = 1234;
    state.baselines.insert(
        "test".into(),
        UsageBaseline {
            day: "2026-06-27".into(),
            total: 7,
        },
    );
    let mut archived = state.active.clone().unwrap();
    archived.graduated = true;
    archived.stage = 2;
    archived.line.rarity = Rarity::Rare;
    state.collection.push(archived);
    state.pin(Some(1));
    let json = serde_json::to_value(&state).unwrap();
    let restored: Companion = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(serde_json::to_value(restored).unwrap(), json);
}

// An empty observation is represented by no provider row; empty IDs cannot replace a ledger.
// upstream: CompanionTests.swift::testEmptyUsageSnapshotDoesNotRebaseDailyLedger
#[test]
fn empty_observation_does_not_rebase_usage() {
    let mut state = Companion::default();
    state.observe_usage("test", "2026-10-31", 0, true);
    state.observe_usage("test", "2026-10-31", 200, true);
    state.observe_usage("", "2026-10-31", 0, true);
    assert_eq!(state.baselines["test"].total, 200);
    assert_eq!(state.egg_tokens, 200);
    state.observe_usage("test", "2026-10-31", 250, true);
    assert_eq!(state.egg_tokens, 250);
    assert_eq!(state.lifetime_tokens, 250);
}

// upstream: CompanionTests.swift::testLegacyAggregateLedgerSeedsProviderMapWithoutRetrospectiveCredit
#[test]
fn legacy_aggregate_does_not_retroactively_credit_providers() {
    let mut state: Companion = serde_json::from_value(serde_json::json!({
        "lifetime_tokens": 10_000, "claimed_today_tokens": 9_000,
    }))
    .unwrap();
    state.observe_usage("codex", "2026-10-31", 500, true);
    assert_eq!(state.lifetime_tokens, 10_000);
    assert_eq!(state.baselines["codex"].total, 500);
    state.observe_usage("codex", "2026-10-31", 700, true);
    assert_eq!(state.lifetime_tokens, 10_200);
}

// upstream: CompanionTests.swift::testDexResolveChainNamesOfflineFallback
#[test]
fn missing_archived_chain_names_resolve_without_network() {
    let mut state = super::economy::hatched();
    let mut mon = state.active.take().unwrap();
    mon.line.names.clear();
    mon.stage = 2;
    mon.graduated = true;
    state.collection.push(mon);
    assert_eq!(
        state
            .dex_species()
            .iter()
            .map(|cell| (cell.id, cell.name.as_str()))
            .collect::<Vec<_>>(),
        [(1, "#1"), (2, "#2"), (3, "#3")]
    );
}

// upstream: UnownTests.swift::testDexDeduplicatesPerLetterAndScopesShinyAndRaisingFlags
#[test]
fn unown_forms_scope_ownership_shiny_and_raising() {
    let line = EvoLine {
        base_id: 201,
        tree: node(201, vec![]),
        rarity: Rarity::Common,
        names: BTreeMap::new(),
    };
    let mut state = Companion::default();
    state.egg_tokens = state.progress().1;
    state.hatch_with_disguise_roll(line, false);
    let mut template = state.active.take().unwrap();
    template.form = Some(forms::Form("a".into()));
    template.shiny = false;
    let mut b = template.clone();
    b.form = Some(forms::Form("b".into()));
    let mut shiny_b = b.clone();
    shiny_b.shiny = true;
    let mut c = template.clone();
    c.form = Some(forms::Form("c".into()));
    state.collection = vec![template, b, shiny_b];
    state.active = Some(c);
    assert_eq!(state.collection.len(), 3);
    assert_eq!(state.dex_species().len(), 1);
    assert!(state.dex_species()[0].shiny);
    assert!(state.dex_species()[0].raising);
    let appearances = state.form_species(201);
    assert_eq!(
        appearances
            .iter()
            .map(|a| a.form.as_ref().unwrap().0.as_str())
            .collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
    assert_eq!(
        appearances.iter().map(|a| a.shiny).collect::<Vec<_>>(),
        [false, true, false]
    );
    assert_eq!(
        appearances.iter().map(|a| a.raising).collect::<Vec<_>>(),
        [false, false, true]
    );
    assert_eq!(
        appearances.iter().map(|a| a.count).collect::<Vec<_>>(),
        [1, 2, 1]
    );
    assert!(state.owns_species(201, Some(&forms::Form("c".into()))));
    assert!(!state.owns_species(201, Some(&forms::Form("z".into()))));
}

// upstream: PokemonFormTests.swift::testEvolutionNamesTheFormLikeHatchAndGraduation
#[test]
fn evolution_event_preserves_nondefault_form_name() {
    let mut state = Companion::default();
    state.egg_tokens = state.progress().1;
    let line = EvoLine {
        base_id: 422,
        tree: node(422, vec![node(423, vec![])]),
        rarity: Rarity::Common,
        names: BTreeMap::from([
            (422, BTreeMap::from([("en".into(), "Shellos".into())])),
            (423, BTreeMap::from([("en".into(), "Gastrodon".into())])),
        ]),
    };
    state.prepare_egg(line);
    state.pending_form = Some(forms::Form("east".into()));
    state.hatch_prepared();
    let events = state.add_growth(state.progress().1);
    let mon = state.active.as_ref().unwrap();
    assert_eq!(mon.species_id(), 423);
    assert_eq!(events, [Event::Evolved(mon.name())]);
    assert_ne!(mon.name(), "Gastrodon");
    assert_eq!(mon.form.as_ref().unwrap().0, "east");
}

// Metadata language selection is shared; Shark keeps its existing interface language.
// upstream: PokemonNameLocalizationTests.swift::testSelectedLanguageThenEnglishWithCurrentAndLegacyCodes
#[test]
fn translated_name_selection_supports_current_and_legacy_codes() {
    let names = BTreeMap::from([
        ("en".into(), "Tackle".into()),
        ("ko".into(), "몸통박치기".into()),
        ("ja-Hrkt".into(), "たいあたり".into()),
        ("ja".into(), "体当たり".into()),
        ("pt_BR".into(), "Investida".into()),
    ]);
    for (codes, expected) in [
        (vec!["ko"], "몸통박치기"),
        (vec!["ja-hrkt", "ja"], "たいあたり"),
        (vec!["pt-br", "pt"], "Investida"),
        (vec!["de"], "Tackle"),
    ] {
        assert_eq!(
            evolution::resolve_name(&names, &codes).as_deref(),
            Some(expected)
        );
    }
    assert_eq!(
        evolution::resolve_name(
            &BTreeMap::from([
                ("ko".into(), " \n".into()),
                ("en".into(), " Tackle ".into())
            ]),
            &["ko"]
        )
        .as_deref(),
        Some("Tackle")
    );
    assert_eq!(
        evolution::resolve_name(&BTreeMap::from([("fr".into(), "Charge".into())]), &["ko"]),
        None
    );
}
// upstream: PokemonNameLocalizationTests.swift::testParserPreservesFutureLanguagesAndIgnoresEmptyValues
#[test]
fn language_map_normalization_preserves_future_languages() {
    let names = evolution::normalized_names(&BTreeMap::from([
        ("ko".into(), "몸통박치기".into()),
        ("en".into(), "Tackle".into()),
        ("it".into(), "Azione".into()),
        ("pt_BR".into(), "Investida".into()),
        ("".into(), "Bad".into()),
        ("fr".into(), " ".into()),
    ]));
    assert_eq!(names["it"], "Azione");
    assert_eq!(names["pt-br"], "Investida");
    assert!(!names.contains_key(""));
    assert!(!names.contains_key("fr"));
    assert_eq!(
        evolution::resolve_name(&names, &["it"]).as_deref(),
        Some("Azione")
    );
    assert_eq!(
        evolution::resolve_name(&names, &["not-yet-supported"]).as_deref(),
        Some("Tackle")
    );
}

fn partial_plan() -> Companion {
    let mut state = super::economy::hatched();
    let mon = state.active.as_mut().unwrap();
    mon.path = vec![1, 2];
    mon.stage = 1;
    state
}
fn assert_recovered_plan(mut state: Companion) {
    let seed = state.random_seed;
    state.sanitize();
    let mon = state.active.unwrap();
    assert_eq!(mon.path, [1, 2, 3]);
    assert_eq!(mon.reached_species().collect::<Vec<_>>(), [1, 2]);
    assert_eq!(state.random_seed, seed);
}
// Rust persists its canonical tree and expands the complete plan while preserving reached stages.
// upstream: ModelLogicTests.swift::testMonStateLegacyDecodeUsesRealizedPathAsPlan
#[test]
fn legacy_realized_prefix_recovers_complete_plan() {
    let saved = serde_json::to_value(partial_plan()).unwrap();
    assert_recovered_plan(serde_json::from_value(saved).unwrap());
}
// upstream: ModelLogicTests.swift::testMonStateEmptySavedPlanUsesRealizedPath
#[test]
fn empty_saved_separate_plan_cannot_erase_realized_prefix() {
    let mut saved = serde_json::to_value(partial_plan()).unwrap();
    saved["active"]["planned_path"] = serde_json::json!([]);
    assert_recovered_plan(serde_json::from_value(saved).unwrap());
}
// upstream: ModelLogicTests.swift::testMonStateEmptyInitialPlanUsesRealizedPath
#[test]
fn incomplete_initial_plan_recovers_without_reroll() {
    assert_recovered_plan(partial_plan());
}

// upstream: CompanionDisplayStateTests.swift::testEggProgressAndTokensToHatch
#[test]
fn egg_progress_and_remaining_hatch_usage() {
    let mut state = Companion::default();
    assert!(state.active.is_none());
    assert_eq!(state.progress(), (0, Balance::EGG_HATCH_THRESHOLD));
    state.observe_usage("test", "day", 0, true);
    let part = Balance::EGG_HATCH_THRESHOLD * 2 / 5;
    state.observe_usage("test", "day", part, true);
    let (used, threshold) = state.progress();
    assert!((used as f64 / threshold as f64 - 0.4).abs() < 0.001);
    assert_eq!(threshold - used, Balance::EGG_HATCH_THRESHOLD - part);
    assert!(used > 0);
}
// Rust persists complete trees, so offline usage catches up immediately without a line request.
// upstream: CompanionTests.swift::testUsageAccruesWhileLineUnloadedThenEvolvesOnLoad
#[test]
fn offline_persisted_tree_never_loses_growth_delta() {
    let state = super::economy::hatched();
    let mut restored: Companion =
        serde_json::from_value(serde_json::to_value(state).unwrap()).unwrap();
    restored.add_growth(300_000_000);
    let mon = restored.active.unwrap();
    assert_eq!(mon.stage, 1);
    assert_eq!(mon.tokens, 175_000_000);
}
// Rust repairs unsupported persisted species before network work at its save trust boundary.
// upstream: CompanionTests.swift::testLineLoadMigratesPersistedUnsupportedEvolution
#[test]
fn persisted_unsupported_evolution_repairs_to_last_supported_species() {
    let mut state = super::economy::hatched();
    let mon = state.active.as_mut().unwrap();
    mon.line.base_id = 56;
    mon.line.tree = node(56, vec![node(57, vec![node(979, vec![])])]);
    mon.path = vec![56, 57, 979];
    mon.stage = 2;
    mon.tokens = 123;
    let mut loaded: Companion =
        serde_json::from_value(serde_json::to_value(state).unwrap()).unwrap();
    loaded.sanitize();
    let mon = loaded.active.unwrap();
    assert_eq!(mon.path, [56, 57]);
    assert_eq!(mon.stage, 1);
    assert_eq!(mon.tokens, 123);
    assert_eq!(mon.line.tree.depth(), 2);
}

// upstream: CompanionTests.swift::testEggHatchesViaRESTFallbackWhenIndexDown
#[test]
fn rest_sampler_hatches_when_graphql_index_is_down() {
    let mut random = 7;
    let id = api::choose_base(
        Err("GraphQL offline".into()),
        None,
        &Default::default(),
        &mut random,
        |id| {
            Ok(Some(api::BaseSpecies {
                id,
                capture_rate: 255,
            }))
        },
    )
    .unwrap();
    let mut state = Companion::default();
    state.egg_tokens = state.progress().1;
    state.hatch_with_disguise_roll(
        EvoLine {
            base_id: id,
            tree: node(id, vec![]),
            rarity: Rarity::Common,
            names: BTreeMap::new(),
        },
        false,
    );
    assert!(state.active.is_some());
    assert_eq!(state.egg_tokens, 0);
}

// Rust legacy aliases are supported inside the Rust save envelope; Swift envelopes remain distinct.
// upstream: PokemonFormTests.swift::testSavesWrittenBeforeGenericFormsKeepTheirUnownLetters
#[test]
fn legacy_unown_form_aliases_survive_generic_form_reencoding() {
    let line = EvoLine {
        base_id: 201,
        tree: node(201, vec![]),
        rarity: Rarity::Common,
        names: BTreeMap::new(),
    };
    let mut state = Companion::default();
    state.egg_tokens = state.progress().1;
    state.hatch_with_disguise_roll(line.clone(), false);
    state.active.as_mut().unwrap().form = Some(forms::Form("z".into()));
    let mut archived = state.active.clone().unwrap();
    archived.form = Some(forms::Form("q".into()));
    state.collection.push(archived);
    state.pending_line = Some(line);
    state.pending_form = Some(forms::Form("k".into()));
    state.representative = Some(201);
    state.representative_form = Some(forms::Form("q".into()));
    let mut json = serde_json::to_value(state).unwrap();
    let map = json.as_object_mut().unwrap();
    for (new, old) in [
        ("pending_form", "pending_unown_form"),
        ("representative_form", "representative_unown_form"),
    ] {
        let value = map.remove(new).unwrap();
        map.insert(old.into(), value);
    }
    let map = json["active"].as_object_mut().unwrap();
    let value = map.remove("form").unwrap();
    map.insert("unown_form".into(), value);
    let map = json["collection"][0].as_object_mut().unwrap();
    let value = map.remove("form").unwrap();
    map.insert("unown_form".into(), value);
    let restored: Companion = serde_json::from_value(json).unwrap();
    assert_eq!(restored.pending_form, Some(forms::Form("k".into())));
    assert_eq!(
        restored.active.as_ref().unwrap().form,
        Some(forms::Form("z".into()))
    );
    assert_eq!(restored.collection[0].form, Some(forms::Form("q".into())));
    assert_eq!(restored.representative_form, Some(forms::Form("q".into())));
    let round = serde_json::to_value(restored).unwrap();
    assert_eq!(round["active"]["form"], "z");
    assert!(round["active"].get("unown_form").is_none());
    assert!(round.get("pending_unown_form").is_none());
}

// upstream: PokemonFormTests.swift::testUnnamedUsualLooksUseLocalizedNormalForm
#[test]
fn unnamed_usual_looks_use_normal_form_label() {
    for id in [351, 479, 646, 649] {
        assert_eq!(
            forms::label(id, None, None, "fr").as_deref(),
            Some("Forme Normale")
        );
    }
    assert_eq!(
        forms::label(479, Some(&forms::Form("normal".into())), None, "de").as_deref(),
        Some("Normalform")
    );
    assert_eq!(
        forms::label(479, Some(&forms::Form("heat".into())), None, "fr").as_deref(),
        Some("Heat")
    );
    assert_eq!(forms::display_name("Motisma", 479, None, None), "Motisma");
}
// upstream: PokemonFormTests.swift::testDisplayNameShowsOnlyNonDefaultFormsExceptUnown
#[test]
fn form_labels_use_metadata_then_fallback_and_hide_default_suffixes() {
    let sky = forms::Form("sky".into());
    assert_eq!(
        forms::label(492, Some(&sky), None, "fr").as_deref(),
        Some("Sky")
    );
    let names = BTreeMap::from([
        ("fr".into(), "Forme Céleste".into()),
        ("en".into(), "Sky Forme".into()),
    ]);
    assert_eq!(
        forms::label(492, Some(&sky), Some(&names), "fr").as_deref(),
        Some("Forme Céleste")
    );
    assert_eq!(
        forms::label(201, Some(&forms::Form("question".into())), None, "fr").as_deref(),
        Some("?")
    );
    assert_eq!(forms::label(25, Some(&sky), None, "fr"), None);
    assert_eq!(
        forms::display_name("Shaymin", 492, Some(&sky), Some("Forme Céleste")),
        "Shaymin [Forme Céleste]"
    );
    assert_eq!(
        forms::display_name(
            "Shaymin",
            492,
            Some(&forms::Form("land".into())),
            Some("Forme Terrestre")
        ),
        "Shaymin"
    );
    assert_eq!(
        forms::display_name("Zarbi", 201, Some(&forms::Form("a".into())), None),
        "Zarbi [A]"
    );
    assert_eq!(
        forms::display_name("Pikachu", 25, Some(&sky), Some("Sky")),
        "Pikachu"
    );
}
// Root's shared provider boundary ignores carrier rows that do not report today's usage.
// upstream: CompanionTests.swift::testCarrierWithoutTodayDataDoesNotRebaseProviderLedger
#[test]
fn carrier_without_today_data_preserves_provider_ledger() {
    let mut state = Companion::default();
    state.observe_usage("codex", "2026-10-31", 0, true);
    state.observe_usage("codex", "2026-10-31", 2000, true);
    state.observe_usage("codex", "2026-10-30", 0, true);
    assert_eq!(state.lifetime_tokens, 2000);
    assert_eq!(state.baselines["codex"].total, 2000);
    state.observe_usage("codex", "2026-10-31", 2200, true);
    assert_eq!(state.lifetime_tokens, 2200);
}

// upstream: UnownTests.swift::testDetailIndividualsAndRaisingFlagAreScopedToExactLetter
#[test]
fn individual_profiles_and_raising_are_scoped_to_exact_unown_letter() {
    let line = EvoLine {
        base_id: 201,
        tree: node(201, vec![]),
        rarity: Rarity::Common,
        names: BTreeMap::new(),
    };
    let mut state = Companion::default();
    state.egg_tokens = state.progress().1;
    state.hatch_with_disguise_roll(line, false);
    let mut a = state.active.clone().unwrap();
    a.form = Some(forms::Form("a".into()));
    a.profile.as_mut().unwrap().instance_id = "a".into();
    let mut b = a.clone();
    b.form = Some(forms::Form("b".into()));
    b.profile.as_mut().unwrap().instance_id = "b".into();
    let mut shiny_b = b.clone();
    shiny_b.shiny = true;
    shiny_b.profile.as_mut().unwrap().instance_id = "shiny-b".into();
    state.collection = vec![a, b, shiny_b];
    let active = state.active.as_mut().unwrap();
    active.form = Some(forms::Form("b".into()));
    active.profile.as_mut().unwrap().instance_id = "active-b".into();
    assert_eq!(
        state
            .form_species(201)
            .iter()
            .map(|form| form.raising)
            .collect::<Vec<_>>(),
        [false, true]
    );
    assert_eq!(state.individuals_for_form(201, None).len(), 1);
    let individuals = state.individuals_for_form(201, Some(&forms::Form("b".into())));
    assert_eq!(individuals.len(), 3);
    assert!(std::ptr::eq(individuals[0], state.active.as_ref().unwrap()));
    assert!(
        individuals
            .iter()
            .all(|mon| mon.form.as_ref().unwrap().0 == "b" && mon.profile.is_some())
    );
    assert_eq!(
        individuals
            .iter()
            .map(|mon| mon.profile.as_ref().unwrap().instance_id.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3
    );
    assert!(
        state
            .individuals_for_form(201, Some(&forms::Form("question".into())))
            .is_empty()
    );
}
