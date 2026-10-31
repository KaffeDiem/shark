use super::super::pricing::*;
use super::*;
// upstream: ModelPricingTests.swift::testCurrentOpenAIStandardRates
#[test]
fn openai_rates() {
    for (id, r) in [
        ("gpt-6-astra", [10., 50., 12.5, 1.]),
        ("gpt-6-sol", [2., 10., 2.5, 0.2]),
        ("gpt-6.1-sol", [2., 10., 2.5, 0.1]),
        ("gpt-6-luna", [0.1, 0.5, 0.125, 0.01]),
        ("gpt-5.6-sol", [4., 20., 5., 0.4]),
        ("gpt-5.6-terra", [2., 12., 2.5, 0.2]),
        ("gpt-5.6-luna", [0.2, 1.2, 0.25, 0.02]),
        ("gpt-5.5", [5., 30., 0., 0.5]),
        ("gpt-5.3-codex", [1.75, 14., 0., 0.175]),
        ("gpt-5.1-codex", [1.25, 10., 0., 0.125]),
    ] {
        assert_eq!(rate(id), Some(Rate(r)))
    }
}
// upstream: ModelPricingTests.swift::testCurrentClaudeStandardRates
#[test]
fn claude_rates() {
    for (id, r) in [
        ("claude-opus-5", [5., 25., 6.25, 0.5]),
        ("claude-opus-5-5", [4., 20., 5., 0.2]),
        ("claude-sonnet-5", [2., 10., 2.5, 0.2]),
        ("claude-sonnet-5-5", [2., 10., 2.5, 0.1]),
    ] {
        assert_eq!(rate(id), Some(Rate(r)))
    }
}
// upstream: ModelPricingTests.swift::testModelsReadFromProviderLogsAreAllPriced
#[test]
fn known_log_identities_priced() {
    for m in [
        "claude-opus-5",
        "claude-opus-5-5",
        "claude-sonnet-5",
        "claude-sonnet-5-5",
        "claude-opus-4-8",
        "claude-opus-4-7",
        "claude-sonnet-4-6",
        "claude-haiku-4-5",
        "claude-fable-5",
        "claude-fable-5-1",
    ] {
        assert!(estimate(m, 2, 175, 26939, 42676).unwrap() > 0., "{m}")
    }
}
// upstream: ModelPricingTests.swift::testUnpricedModelIsReportedOnlyOnFirstSighting
#[test]
fn unpriced_sighting_normalizes() {
    assert!(first_unpriced_sighting("test-first-unpriced"));
    assert!(!first_unpriced_sighting("test-first-unpriced"));
    assert!(!first_unpriced_sighting(" ANTHROPIC/TEST-FIRST-UNPRICED "))
}
// upstream: ModelPricingTests.swift::testAggregatingAnUnpricedModelReportsItButNotASourceUnavailableRow
#[test]
fn bucket_records_unknown_only_when_missing_rate() {
    let mut b = Bucket::default();
    b.add(&Entry {
        model: "test-bucket-unknown".into(),
        input: 10,
        ..Entry::default()
    });
    b.add(&Entry {
        model: "test-bucket-source-unavailable".into(),
        input: 10,
        cost_unavailable: Some(true),
        ..Entry::default()
    });
    assert!(!first_unpriced_sighting("test-bucket-unknown"));
    assert!(first_unpriced_sighting("test-bucket-source-unavailable"))
}
// upstream: ModelPricingTests.swift::testUnknownNamesNeverBorrowFamilyPrices
#[test]
fn unknown_names_never_guess() {
    for m in [
        "gpt-5.3-codex-spark",
        "gpt-99",
        "codex",
        "codex-auto-review",
        "o3",
        "o4",
        "grok-codex-next",
        "claude-opus-4-99",
        "claude-fable-6",
        "gemini-99-pro",
        "custom/claude-opus-4-8",
        "antigravity/claude-opus-4-8",
        "gpt-5.5-pro",
        "gpt-5.5-2026-99-99",
    ] {
        assert_eq!(estimate(m, 100, 20, 0, 40), None)
    }
}
// upstream: ModelPricingTests.swift::testExplicitAliasesAndProviderPrefixes
#[test]
fn aliases_and_prefixes() {
    assert_eq!(rate("openai/gpt-5.4-2026-03-05"), rate("gpt-5.4"));
    assert_eq!(
        rate(" ANTHROPIC/CLAUDE-FABLE-5-1 "),
        Some(Rate([10., 50., 12.5, 0.25]))
    );
    assert_eq!(rate("models/gemini-2.5-pro"), rate("gemini-2.5-pro"))
}
// upstream: ModelPricingTests.swift::testRequestContextThresholdIncludesCacheRead
#[test]
fn request_context_threshold() {
    close(estimate("gpt-5.5", 2000, 1000, 0, 270000).unwrap(), 0.175);
    close(estimate("gpt-5.5", 2001, 1000, 0, 270000).unwrap(), 0.33501);
    close(
        estimate("gpt-6-luna", 272001, 1000, 0, 0).unwrap(),
        0.0551502,
    );
    for m in ["gpt-6-sol", "gpt-6.1-sol"] {
        close(estimate(m, 272001, 1000, 0, 0).unwrap(), 1.103004)
    }
    close(estimate("gpt-5.3-codex", 300000, 0, 0, 0).unwrap(), 0.525)
}
// upstream: ModelPricingTests.swift::testGemini25FlashLiteUsesOfficialTextRates
#[test]
fn generic_flash_lite_price_table() {
    assert_eq!(
        rate("models/gemini-2.5-flash-lite"),
        Some(Rate([0.1, 0.4, 0., 0.01]))
    );
    close(
        estimate("gemini-2.5-flash-lite", 1000000, 0, 0, 0).unwrap(),
        0.1,
    );
    close(
        estimate("gemini-2.5-flash-lite", 0, 1000000, 0, 0).unwrap(),
        0.4,
    );
    close(
        estimate("gemini-2.5-flash-lite", 0, 0, 0, 1000000).unwrap(),
        0.01,
    );
    assert_eq!(estimate("gemini-2.5-flash-lite", 0, 0, 1, 0), None);
    assert_eq!(rate("gemini-3-flash-lite"), None)
}
// upstream: ModelPricingTests.swift::testGeminiTextCacheAndLongContext
#[test]
fn generic_gemini_price_table() {
    assert_eq!(rate("gemini-2.5-flash"), Some(Rate([0.3, 2.5, 0., 0.03])));
    close(
        estimate("gemini-2.5-pro", 100000, 1000, 0, 100000).unwrap(),
        0.1475,
    );
    close(
        estimate("gemini-2.5-pro", 100001, 1000, 0, 100000).unwrap(),
        0.2900025,
    )
}
// upstream: ModelPricingTests.swift::testUnsupportedCacheWriteRateIsUnavailableInsteadOfFree
#[test]
fn unsupported_write_not_free() {
    for m in ["gpt-5.5", "gpt-5.3-codex", "gemini-2.5-pro"] {
        assert_eq!(estimate(m, 0, 0, 100000, 0), None)
    }
    close(estimate("gpt-6-astra", 0, 0, 100000, 0).unwrap(), 1.25)
}
// upstream: ModelPricingTests.swift::testCacheWriteAndInvalidBuckets
#[test]
fn valid_and_invalid_buckets() {
    close(
        estimate("gpt-5.6-luna", 100000, 1000, 10000, 100000).unwrap(),
        0.0257,
    );
    close(estimate("gpt-6-luna", 100000, 0, 0, 0).unwrap(), 0.01);
    close(
        estimate("gpt-6.1-sol", 100000, 1000, 10000, 100000).unwrap(),
        0.245,
    );
    assert_eq!(token(&json!(-1)), 0);
    assert_eq!(estimate("gpt-5.5", 0, 0, 0, 0), Some(0.))
}
