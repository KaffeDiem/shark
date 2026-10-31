use crate::pokemon::format::*;
use crate::pokemon::{Balance, Companion};
// upstream: PokeTokenBarTests.swift::testCompactPromotesUnitsAtRoundedBoundaries
#[test]
fn compact_promotes_rounded_units() {
    for (value, expected) in [
        (999, "999"),
        (1000, "1K"),
        (999949, "999.9K"),
        (999950, "1M"),
        (999999, "1M"),
        (1000000, "1M"),
        (999949999, "999.9M"),
        (999950000, "1B"),
        (999999999, "1B"),
        (1000000000, "1B"),
        (1240000000, "1.24B"),
    ] {
        assert_eq!(compact(value), expected);
        assert_eq!(signed_compact(-(value as i64)), format!("-{expected}"))
    }
}
// upstream: PokeTokenBarTests.swift::testCompact
#[test]
fn compact_examples() {
    for (value, expected) in [
        (0, "0"),
        (987, "987"),
        (12345, "12.3K"),
        (190612940, "190.6M"),
        (1240000000, "1.24B"),
        (1000000, "1M"),
    ] {
        assert_eq!(compact(value), expected)
    }
}
// upstream: DifficultyTests.swift::testPercentFormatting
#[test]
fn difficulty_percent_preserves_small_values() {
    for (value, expected) in [
        (1., "100%"),
        (0.5, "50%"),
        (20., "2000%"),
        (0.05, "5.0%"),
        (0.0001, "0.01%"),
    ] {
        assert_eq!(difficulty_percent(value), expected)
    }
    assert_ne!(difficulty_percent(0.0001), difficulty_percent(0.0005))
}
// upstream: DifficultyTests.swift::testFirstRunEggHintShowsTheScaledHatchPoint
#[test]
fn scaled_first_egg_hint_all_languages() {
    let mut companion = Companion::default();
    companion.growth_difficulty = 0.1;
    assert_eq!(companion.progress().1, 500000);
    assert_eq!(
        Companion::default().progress().1,
        Balance::EGG_HATCH_THRESHOLD
    );
    for language in Language::ALL {
        let hint = language.egg_hint(&compact(companion.progress().1));
        assert!(hint.contains("500K"));
        assert!(!hint.contains("5M"))
    }
}
// upstream: ShinyCharmTests.swift::testShinyNotificationShowsTheOddsTheHatchUsed
#[test]
fn shiny_hatch_copy_uses_actual_odds_all_languages() {
    for language in Language::ALL {
        let text = language.shiny_hatch("P", 48);
        assert!(text.contains("48"));
        assert!(!text.contains("64"));
        assert!(text.contains('P'))
    }
}
// upstream: ShinyCharmTests.swift::testShinyDittoRevealDoesNotClaimFixedOdds
#[test]
fn ditto_reveal_copy_never_claims_fixed_odds() {
    for language in Language::ALL {
        let body = language.shiny_ditto("P");
        assert!(!body.contains("64"));
        assert!(!body.contains("48"));
        assert!(body.contains('P'))
    }
}
// upstream: RareCandyTests.swift::testTitleLocalizedAllLanguages
#[test]
fn candy_title_contains_quantity_all_languages() {
    for language in Language::ALL {
        let title = language.candy_title("Rare Candy", 3);
        assert!(!title.is_empty());
        assert!(title.contains('3'));
        assert!(title.contains("Rare Candy"));
        let body = language.candy_body("Window");
        assert!(body.contains("Window"))
    }
}
// upstream: RareCandyTests.swift::testTitleIncludesCountAndItem
#[test]
fn korean_candy_title_count_and_item() {
    let one = Language::Ko.candy_title("이상한 사탕", 1);
    let five = Language::Ko.candy_title("이상한 사탕", 5);
    assert!(one.contains("이상한 사탕"));
    assert!(one.contains("1개"));
    assert!(five.contains("5개"))
}
// upstream: RareCandyTests.swift::testBodyPrefixesWindowNameClaudeAndCodex
#[test]
fn korean_candy_body_prefixes_exact_provider_window() {
    for window in [
        "Claude 5시간 세션",
        "Claude 주간",
        "Codex 5시간 세션",
        "Codex 주간",
    ] {
        assert!(
            Language::Ko
                .candy_body(window)
                .starts_with(&format!("{window} 토큰 한도를 다 채웠"))
        )
    }
}
