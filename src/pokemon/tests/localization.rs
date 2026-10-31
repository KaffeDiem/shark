use super::super::{
    Item, NATURES,
    format::Language,
    names_client::{Kind, parse},
};
// upstream: CompanionTests.swift::testGermanNatureNamesMatchOfficialMainlineNames
#[test]
fn german_natures_match_mainline_names() {
    let expected = [
        "Robust", "Solo", "Mutig", "Hart", "Frech", "Kühn", "Sanft", "Locker", "Pfiffig", "Lasch",
        "Scheu", "Hastig", "Ernst", "Froh", "Naiv", "Mäßig", "Mild", "Ruhig", "Zaghaft", "Hitzig",
        "Still", "Zart", "Forsch", "Sacht", "Kauzig",
    ];
    assert_eq!(
        NATURES.map(|nature| Language::De.nature_name(nature)),
        expected
    );
}
// upstream: CompanionTests.swift::testRussianNatureNamesFollowDocumentedCommunityConvention
#[test]
fn russian_natures_match_upstream_community_convention() {
    let expected = [
        "Выносливый",
        "Одинокий",
        "Храбрый",
        "Непреклонный",
        "Непослушный",
        "Смелый",
        "Послушный",
        "Расслабленный",
        "Проказливый",
        "Беспечный",
        "Робкий",
        "Торопливый",
        "Серьёзный",
        "Весёлый",
        "Наивный",
        "Скромный",
        "Мягкий",
        "Тихий",
        "Застенчивый",
        "Опрометчивый",
        "Спокойный",
        "Кроткий",
        "Нахальный",
        "Внимательный",
        "Ловкий",
    ];
    assert_eq!(
        NATURES.map(|nature| Language::Ru.nature_name(nature)),
        expected
    );
}
// upstream: CompanionTests.swift::testGermanItemNamesUseOfficialMainlineTerms
#[test]
fn german_items_use_mainline_names() {
    assert_eq!(Language::De.item_name(Item::RareCandy), "Sonderbonbon");
    assert_eq!(Language::De.item_name(Item::ShinyCharm), "Schillerpin");
}
// upstream: CompanionTests.swift::testNatureNamesComplete
#[test]
fn all_twenty_five_natures_are_distinct_in_every_language() {
    assert_eq!(NATURES.len(), 25);
    for lang in Language::ALL {
        let names = NATURES.map(|nature| lang.nature_name(nature));
        assert_eq!(
            names
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            25
        );
        assert!(names.iter().all(|name| !name.is_empty()));
    }
}
// upstream: CompanionTests.swift::testLocalizationIncludesDelayedHatchRetry
#[test]
fn delayed_hatch_copy_matches_every_supported_language() {
    let expected = [
        "⏳ 부화가 지연되고 있어요. 다음 새로고침에서 다시 시도해요",
        "⏳ Hatching is delayed — retrying on the next refresh",
        "⏳ 孵化が遅れています。次回の更新時に再試行します",
        "⏳ La eclosión se retrasa; se reintentará en la próxima actualización",
        "⏳ L’éclosion est retardée — nouvel essai au prochain rafraîchissement",
        "⏳ A eclosão está atrasada — nova tentativa na próxima atualização",
        "⏳ Das Schlüpfen verzögert sich — neuer Versuch bei der nächsten Aktualisierung",
        "⏳ Вылупление задерживается — повторим при следующем обновлении",
    ];
    assert_eq!(Language::ALL.map(Language::egg_delayed), expected);
}
// upstream: PreciousCompanionGuardTests.swift::testLocalizationStringsCoverAllSevenLanguages
#[test]
fn valuable_confirmation_copy_is_complete_in_all_eight_languages() {
    for language in Language::ALL {
        assert!(language.valuable_warning(false).starts_with("⚠️"));
        assert!(language.valuable_warning(true).starts_with("⚠️"));
        assert!(!language.send_off().is_empty());
    }
    assert!(Language::Ko.valuable_warning(false).contains("전설 포켓몬"));
    assert_eq!(Language::Ko.send_off(), "놓아주기");
    assert!(Language::En.valuable_warning(false).contains("Legendary"));
    assert_eq!(Language::En.send_off(), "Send off");
}
// REST returns every language instead of using Swift's GraphQL language allowlist.
// upstream: ModelLogicTests.swift::testFetchedLanguageCodesMatchAppLanguageAPICodesWithoutDuplicates
#[test]
fn metadata_preferred_codes_are_unique_and_rest_preserves_all_languages() {
    let codes = Language::ALL
        .into_iter()
        .flat_map(Language::api_codes)
        .copied()
        .collect::<Vec<_>>();
    assert_eq!(
        codes,
        [
            "ko", "en", "ja-hrkt", "ja", "es", "fr", "pt-br", "pt", "de", "ru"
        ]
    );
    assert_eq!(
        codes
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        codes.len()
    );
    let mut rows = codes
        .iter()
        .map(|code| serde_json::json!({"name":format!("Label {code}"),"language":{"name":code}}))
        .collect::<Vec<_>>();
    rows.push(serde_json::json!({"name":"Azione","language":{"name":"it"}}));
    let names = parse(Kind::Move, &serde_json::json!({"names":rows})).unwrap();
    for code in codes {
        assert!(names.contains_key(code));
    }
    assert_eq!(names["it"], "Azione");
}
