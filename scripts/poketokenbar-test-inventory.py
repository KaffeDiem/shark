#!/usr/bin/env python3
"""Audit translated XCTest cases against an explicit, pinned upstream checkout.

Usage: python3 scripts/poketokenbar-test-inventory.py /path/to/PokeTokenBar
Markers have the form // upstream: Filename.swift::testMethodName.
No source-only check can establish assertion equivalence: migrated means a Rust
case is present; test execution and review remain separately required.
"""
import csv
import pathlib
import re
import subprocess
import sys
from collections import Counter

PIN = '32ebe4cac2aca1d298d71e7cbc70a507015470f9'
repo = pathlib.Path(__file__).resolve().parents[1]
upstream = pathlib.Path(sys.argv[1]).resolve()
revision = subprocess.check_output(['git', '-C', str(upstream), 'rev-parse', 'HEAD'], text=True).strip()
if revision != PIN:
    raise SystemExit(f'Expected upstream {PIN}; found {revision}')
other_providers = ('Antigravity', 'Aside', 'Copilot', 'Cursor', 'Gemini', 'Grok', 'KimiCode', 'Kiro', 'Omp', 'PiUsage', 'LocalAdditional', 'IncrementalStoreScan')
retained_quota = {'AccountSessionKeyTests.swift', 'AdditionalClaudeAccountsTests.swift', 'CredentialSwitchCacheTests.swift', 'KeychainAutoPathTests.swift', 'LiveCredentialCallGateTests.swift', 'SessionKeyLimitsTests.swift', 'SessionKeySettingsRenderingTests.swift', 'CodexRateLimitsProviderTests.swift', 'BinaryLocatorTests.swift', 'NetworkReachabilityTests.swift'}
excluded_cases = {
    ('LocalUsageCacheTests.swift', name) for name in (
        'testPiCacheInvalidatesGrowingFilesAndDedupsAcrossRoots', 'testPiCacheRoundTripsAcrossInstances',
        'testPiReadFailureKeepsOldBlobAndRetriesNextRefresh', 'testPiCacheInvalidatesOutdatedOrMissingParserVersion')
} | {
    ('LocalUsageReaderTests.swift', 'testGeminiParsingClampsAndItsAdditionsStayInRange'),
    ('UsageCostTests.swift', 'testOpenCodeExplicitZeroAndMissingCostThroughParser'),
    ('UsageCostTests.swift', 'testCursorLocalBubbleWithoutCacheSplitIsUnavailable'),
    ('LimitPaceTests.swift', 'testWindowSpanFromAntigravityBucket'),
    *{('UsageStoreTests.swift', name) for name in ('testKiroSnapshotFlowsThroughTodayWeekMonthAndBurn', 'testCursorAuthExpiredClearsWhenThereIsNoCursorLoginAnymore', 'testCursorProviderFlowsThroughAggregation', 'testCursorAndClaudeCombinedAggregation', 'testCursorContributesZeroToTodayCostTotal', 'testCursorOnlyDoesNotSurfaceZeroDollarCostInMenu')},
    *{('CustomScanRootsTests.swift', name) for name in ('testAdditionalProvidersUnionCustomOntoCurated', 'testNilCustomLeavesAdditionalProviderDefaultsUnchanged', 'testGeminiCacheScansMultipleRootsAndDedups', 'testSetCustomScanRootsInvalidatesAdditionalProviderCache')},
}
retained_host = {'UpdateCheckerTests.swift': 'Standalone PokeTokenBar release checks and detached self-upgrader are replaced by Shark installation/update behavior', 'SingleInstanceTests.swift': 'Standalone login-agent process arbitration is replaced by Shark application lifecycle', 'AppLogTests.swift': 'Standalone process file-log flush and clean-exit markers are replaced by Shark diagnostics', 'LogRepeatSuppressorTests.swift': 'Standalone PokeTokenBar file-log repeat suppression is not part of the embedded companion', 'SwiftUIIsolationTests.swift': 'Swift MainActor source lint is replaced by GPUI Context ownership and Rust compilation', 'LanguageSurfaceRegressionTests.swift': 'Shark retains its interface language and settings; the standalone language selector is not embedded', 'LocalizationColumnTests.swift': 'Swift positional localization source scanner is not used by the Rust integration', 'LocalizationInterpolationTests.swift': 'Swift localization source parser is replaced by Rust compile-time format strings and mapped runtime formatter tests', 'LocalizedUILiteralTests.swift': 'Swift UI source-literal lint does not apply to Shark native views'}
retained_quota_cases = {('UsageStoreTests.swift', 'testLimitsAuthExpiredNotSetOnNon401'), ('PokeTokenBarTests.swift', 'testPlanDisplay'), ('PokeTokenBarTests.swift', 'testLimitStatus'), ('LimitPaceTests.swift', 'testWindowSpanFromOAuthLimitEntryKind'), ('PokeTokenBarTests.swift', 'testCredentialParsesSecurityCLIOutputWithTrailingNewline'), ('PokeTokenBarTests.swift', 'testLimitStatusScopedEntries'), ('UsageStoreTests.swift', 'testKeychainDisabledSkipsLimitsWithoutSessionKey'), ('PokeTokenBarTests.swift', 'testCodexRateLimitStatusLegacyNilLimitIdDedup'), ('PokeTokenBarTests.swift', 'testPrioritizedAccountNamesPlacesCurrentUserNameFirstAndUnknownLast'), ('UsageStoreTests.swift', 'testSessionKeyExpiredIsVisibleWhileKeyIsStillStored'), ('UsageStoreTests.swift', 'testLimitsAuthExpiredSetOn401AndClearedOnSuccess'), ('UsageStoreTests.swift', 'testSelectSessionOrganizationSwitchesOrgKeepingTheKey'), ('PokeTokenBarTests.swift', 'testLimitStatusAccountDisplay'), ('PokeTokenBarTests.swift', 'testPrioritizedAccountNamesCaseInsensitiveUserMatching'), ('UsageStoreTests.swift', 'testOAuthExpiryDoesNotMarkSessionKeyExpired'), ('PokeTokenBarTests.swift', 'testFriendlyErrorForRateLimited'), ('UsageStoreTests.swift', 'testAccountOAuthMissingIsDistinguishedFromMalformedCredential'), ('PokeTokenBarTests.swift', 'testValidAccountOAuthIsPreferredOverMCPOnlyEntry'), ('PokeTokenBarTests.swift', 'testPrioritizedAccountNamesPreservesStableOrderForSamePriority'), ('PokeTokenBarTests.swift', 'testPrioritizedAccountNamesWhenCurrentUserIsUnknownKeepsUnknownLast'), ('UsageStoreTests.swift', 'testKeychainDisabledStillFetchesLimitsWhenSessionKeyIsConfigured'), ('PokeTokenBarTests.swift', 'testOAuthProfileIdentityParsing'), ('PokeTokenBarTests.swift', 'testPrioritizedAccountNamesMalformedEmailsDoNotGainEmailPriority'), ('PokeTokenBarTests.swift', 'testNoUIQueryAddsAuthenticationContextAndFailPolicy'), ('PokeTokenBarTests.swift', 'testLimitStatusLegacyEmptyFallsBackToAllEntries'), ('UsageStoreTests.swift', 'testOAuthExpiryIsNotReportedAsSessionKeyExpiry'), ('PokeTokenBarTests.swift', 'testRetryAfterHeaderParsing'), ('UsageStoreTests.swift', 'testRefreshSessionOrganizationsRepopulatesAfterRestart'), ('PokeTokenBarTests.swift', 'testCredentialTreatsPastExpirationAsExpired'), ('PokeTokenBarTests.swift', 'testCredentialParsesMillisecondExpiration'), ('PokeTokenBarTests.swift', 'testNextBackoffDoublesAndCaps'), ('UsageStoreTests.swift', 'testSaveSessionKeyRejectsKeyWithNoVisibleOrganization'), ('PokeTokenBarTests.swift', 'testPrioritizedAccountNamesFullHierarchy'), ('PokeTokenBarTests.swift', 'testCredentialParsesPlanFields'), ('UsageStoreTests.swift', 'testAutoRefreshUsesNoPromptPathManualUsesPromptPath'), ('UsageStoreTests.swift', 'testSaveSessionKeyRejectsMalformedInputWithoutStoring'), ('UsageStoreTests.swift', 'testSessionKeyExpiryClearsOnSuccess'), ('UsageStoreTests.swift', 'testFriendlyLimitErrorMapping'), ('UsageStoreTests.swift', 'testSessionKeyExpiryIsDistinguishedFromOAuthExpiry'), ('PokeTokenBarTests.swift', 'testKeychainQueriesAreAcceptedBySecurityFramework'), ('UsageStoreTests.swift', 'testRefreshSessionOrganizationsDoesNothingWithoutKey'), ('PokeTokenBarTests.swift', 'testPrioritizedAccountNamesHandlesEmptyAndEdgeInputs'), ('PokeTokenBarTests.swift', 'testDataQueryAsksForOneItemAndAccountsQueryAsksForNoData'), ('PokeTokenBarTests.swift', 'testAccountNamesHandlesArrayAndSingleAndDedupes'), ('UsageStoreTests.swift', 'testSaveSessionKeyValidatesAndPicksOrgWithUsage'), ('PokeTokenBarTests.swift', 'testCodexRateLimitStatusMultiBucket'), ('PokeTokenBarTests.swift', 'testCodexRateLimitStatus'), ('UsageStoreTests.swift', 'testClearSessionKeyResetsStateAndStorage')}
retained_host_cases = {('UsageStoreTests.swift', 'testMenuLinesStacksUsageAndLimitsSeparately'), ('UsageStoreTests.swift', 'testMenuBarLimitShowsProviderUsedToday'), ('PokeTokenBarTests.swift', 'testMailtoURLEncodesSubjectAndBody'), ('UsageStoreTests.swift', 'testSystemWakeResumesSuspendedPollingWhenDisplaysAreAwake'), ('UsageStoreTests.swift', 'testRefreshWhileDisplaysAsleepKeepsPollingSuspended'), ('PokeTokenBarTests.swift', 'testMailtoEncodesPlusSign'), ('UsageStoreTests.swift', 'testMenuLinesTokenAndCostStackVertically'), ('UsageStoreTests.swift', 'testBubbleAlertPicksHighestSeverityThenUtilization'), ('UsageStoreTests.swift', 'testRefreshWhileDisplaysAwakeClearsAStaleSuspension'), ('UsageStoreTests.swift', 'testSuspendedPollingProbeStaysSuspendedWhileDisplaysSleep'), ('UsageStoreTests.swift', 'testBubbleDismissUsesTTL'), ('PokeTokenBarTests.swift', 'testMailBodyContainsDiagnostics'), ('UsageStoreTests.swift', 'testMenuTitleReflectsToggles'), ('UsageStoreTests.swift', 'testMenuLimitColorModePersists'), ('UsageStoreTests.swift', 'testFloatingPetSettingsDefaultAndPersistence'), ('UsageStoreTests.swift', 'testSuspendedPollingProbeResumesWhenDisplaysAreAwake'), ('UsageStoreTests.swift', 'testLimitDisplayModePersistsAcrossRestart'), ('UsageStoreTests.swift', 'testMenuBarLimitHidesProviderUnusedToday'), ('UsageStoreTests.swift', 'testMenuTitleShowsLimitPercents'), ('UsageStoreTests.swift', 'testMenuLinesAllCombinations')}
# Standalone NSStatusItem machinery is deliberately not installed in Shark's window.
retained_host.update({
    'OutsideClickMonitorTests.swift': 'Standalone global NSEvent monitor is replaced by GPUI element event subscriptions and Shark overlay dismissal',
    'MenuLimitColoringTests.swift': 'NSAttributedString menu-title substring coloring is replaced by independent native quota row elements',
    'ScrollerLaneTests.swift': 'Swift source scanner for NSScrollView scroller-lane modifiers is replaced by native GPUI scroll layout and mounted layout regressions',
})
retained_host_cases.update({
    ('PopoverNavigationTests.swift', 'testPopoverFooterTogglesFloatingPetWithoutOpeningSettings'),
    ('PerformanceTests.swift', 'testPetFreezesUnderLowPower'),
    *{('SpriteAspectRatioTests.swift', name) for name in ('testMenuBarCanvasTracksFittedWidthAndKeepsFixedHeight', 'testWideSpriteFillsMenuBarHeightUntilTheWidthCap', 'testMenuBarGeometryUnchangedForSquareSprites', 'testBobOffsetLiftsSpriteAndStaysInsideCanvas', 'testComposedMenuBarImageUsesFittedCanvas')},
})
retained_quota_cases.update({
    *{('ModelLogicTests.swift', name) for name in ('testSecondsFormNotTreatedAsMillis','testMillisFormDividedByThousand','testStringFormParsed','testZeroOrMissingExpiryNeverExpires','testRejectsMissingOrEmptyToken','testWindowDisplayName','testSpendControlUsedPercentClamped','testHasVisibleLimitReflectsWindows')},
    ('LimitPaceTests.swift','testWindowSpanFromCodexWindowDurationMinutes'),
    ('RareCandyTests.swift','testCodexWindowClassification'),
})
excluded_cases.add(('RareCandyTests.swift','testEligibleWindowsExcludeGemini'))
retained_host_cases.update({('CompanionTests.swift', name) for name in (
    'testGermanLanguageMetadata','testGermanLanguagePersistsInCompanionState','testGermanPreferredLanguagesMapToGerman',
    'testRussianLanguageMetadata','testRussianLanguagePersistsInCompanionState','testRussianPreferredLanguagesMapToRussian',
    'testDisplayLocaleMatchesLanguageCode','testRelativeTimeFollowsAppLanguageNotSystem','testSystemDefaultLanguageResolves')})
retained_host_cases.update({('UsageStoreTests.swift', name) for name in (
    'testProviderStatusParse','testCodexStatusIgnoresUnrelatedGlobalIncident','testProviderComponentDegradationMapsToIssue','testProviderStatusKeepsPreviousOnFailure','testProviderStatusDisabledClears',
    'testLimitAlertFiresOncePerTierNotEveryRefresh','testLimitAlertDoesNotRefireOnRepeatedSameUtilization','testLimitAlertRearmsAfterDroppingBelowWarn','testLimitAlertTracksWindowsIndependently','testLimitAlertKeyDisambiguatesDuplicateDisplayNames',
    'testWarningStaysBelowCritical','testStoredThresholdsLoadInOrder')})
retained_quota_cases.add(('UsageStoreTests.swift','testLimitWarningFromForecastAtFullUtilization'))
retained_host_cases.update({('UsageStoreTests.swift',name) for name in ('testMenuLimitColorRunsFollowTheMode','testMenuLimitItemsWithoutAPaceUseTheThresholdColors','testMenuLimitColorIsJudgedFromUsageInRemainingMode')})
# Native Rust contracts intentionally eliminate a few Swift-only intermediate states.
adaptations = {
    ('UsageStoreTests.swift', 'testHighestLimitUtilizationIgnoresProviderUnusedToday'): 'User requested both provider quotas remain visible regardless of local token activity; official headline warnings likewise do not hide a provider with zero or yesterday-only local activity',
    **{('LocalUsageParityTests.swift', name): 'Opt-in read-only audit requires an explicitly supplied sanitized corpus instead of implicitly reading the user home; native app publication is separately covered by provider lifecycle tests' for name in ('testFullRefreshPipelineWithRealProviders', 'testPrintRealAggregates', 'testCodexMetadataProbeOnRealRollouts', 'testCodexOrphanedParentDoesNotRescanRealTree', 'testCachePerformance')},
    ('UnownSpriteTests.swift', 'testExistingUnownAndExplicitAKeepLegacyCacheKeys'): 'Implicit and explicit A retain the same Rust cache key; Swift filename spelling is not imported',
    ('SaveSnapshotTests.swift', 'testSnapshotSummariesResolveCurrentAppearanceAcrossLegacyAndEnvelopeFiles'): 'Internal history supports raw Rust companion JSON and envelopes; Swift save layouts are not accepted as Rust saves',
    ('CompanionTests.swift', 'testUsageIncreaseAfterValidDropContinuesEggProgress'): 'Deliberate per-day high-water baseline prevents duplicate growth when logs disappear then reappear',
    ('CompanionTests.swift', 'testLegacyAggregateLedgerSeedsProviderMapWithoutRetrospectiveCredit'): 'Rust retains lifetime balance; an unknown legacy aggregate cannot seed individual provider baselines',
    ('CompanionTests.swift', 'testEmptyUsageSnapshotDoesNotRebaseDailyLedger'): 'Empty observations are represented as absent or invalid provider rows',
    ('CompanionTests.swift', 'testBackwardCompatibleDecode'): 'Required Hardy nature replaces the upstream absent nature for legacy saves',
    ('ModelLogicTests.swift', 'testMonStateRoundTripPreservesDistinctPlannedPath'): 'Full Rust path and stage encode the realized prefix without a separate planned-path field',
    ('ModelLogicTests.swift', 'testMonStateDecodeClampsStageIndexToRealizedPathBounds'): 'Malformed indices recover at the Companion sanitize boundary',
    ('SaveTransferTests.swift', 'testSuggestedFileNameCarriesDate'): 'Shark-branded filename uses a deterministic UTC date instead of a locale-dependent date',
    ('SaveTransferTests.swift', 'testImportDuringHatchStillLoadsTheImportedLine'): 'Rust saves persist validated evolution trees so import restores its line synchronously and separately schedules profile metadata',
    ('SaveTransferTests.swift', 'testImportDuringSpeciesRollDiscardsTheHatch'): 'Species selection and evolution-line fetch share one worker completion with the generation captured before both operations',
    ('UsageEnvironmentTests.swift', 'testTestBinaryIsNotABundledApp'): 'Rust cfg(test) makes environment discovery inert; test exercises injected roots without reading live accounts',
    ('UsageEnvironmentTests.swift', 'testDefaultPathIsInertUnlessBundled'): 'Shark uses its existing injected state home; test builds disable login-shell environment discovery rather than detecting an NSBundle',
    ('UsageCostTests.swift', 'testOptionalLocalCodexCostAudit'): 'Opt-in live-log diagnostic becomes an immutable fixture audit with the same token, cost, and unknown-coverage invariants; tests never inspect user transcripts',
    ('LocalUsageReaderTests.swift', 'testDegenerateParentHintIsNotUsedToNarrowCandidates'): 'Rust uses exact metadata session-ID indexing instead of filename-substring hints; regression asserts degenerate IDs cannot trigger whole-tree full parsing',
    ('ModelPricingTests.swift', 'testCacheWriteAndInvalidBuckets'): 'Negative token values are rejected or folded at the parser boundary; estimated costs accept unsigned disjoint token buckets',

    ('ShopTests.swift', 'testBuyNonPositiveCountIsNoOp'): 'Zero is a no-op; negative quantities cannot inhabit u64 and are rejected at deserialization',
    ('RareCandyTests.swift', 'testBatchCapsAtInventoryAndRejectsNonpositiveAmounts'): 'Unsigned Rust quantity boundary rejects negative values; zero and inventory caps remain runtime assertions',
    ('MintTests.swift', 'testUseMintFromNilNatureSetsValid'): 'Rust nature is a required string; the equivalent legacy empty name is replaced with a valid nature',
    ('ProfileGrowthIntegrationTests.swift', 'testOfflineOverflowCatchesUpWhenLineLoads'): 'Validated evolution trees persist in Rust saves, so offline growth catches up immediately without a network line-load state',
    ('RareCandyTests.swift', 'testCannotUseWhileLineUnloaded'): 'A missing/invalid evolution tree is rejected before item use; Rust cannot hold an active Pokemon without its line',
    ('RareCandyTests.swift', 'testSessionGrantAfterSeed'): 'Each provider establishes its own first-observation baseline instead of granting retroactively for a newly discovered account',
    ('RareCandyTests.swift', 'testWeeklyGrantsFiveCandies'): 'Each provider establishes its own first-observation baseline instead of granting retroactively for a newly discovered account',
    ('RareCandyTests.swift', 'testStaggeredProviderSeedRetroactiveGrant'): 'Regression deliberately fixes the upstream documented retroactive-grant bug through per-provider baselines',
}
adaptations.update({
    ('SpriteAnimationRecoveryTests.swift','testUnsupportedSpeciesLoadsStaticOnceWithoutRetry'): 'Rust explicitly rejects species outside the requested Gen 1–5 catalog before any sprite fetch rather than supporting static artwork beyond Gen 5',
    ('CompanionTests.swift','testCorruptStateFileBackedUpBeforeReset'): 'Shark backs up the whole state file with unique collision-safe names; the companion resets only when no valid internal snapshot can recover it',
    **{('ModelLogicTests.swift',name): 'Rust saves persist a canonical evolution tree and complete planned path; stage preserves the actually reached prefix' for name in ('testMonStateLegacyDecodeUsesRealizedPathAsPlan','testMonStateEmptySavedPlanUsesRealizedPath','testMonStateEmptyInitialPlanUsesRealizedPath')},
    ('CompanionTests.swift','testUsageAccruesWhileLineUnloadedThenEvolvesOnLoad'): 'Validated evolution trees persist in Rust saves, so offline growth evolves immediately rather than waiting on a separate line fetch',
    ('CompanionTests.swift','testLineLoadMigratesPersistedUnsupportedEvolution'): 'Rust sanitizes unsupported saved evolution stages before any metadata fetch',
    ('CompanionTests.swift','testLineLoadPreservesUpdatesMadeWhileProviderIsSuspended'): 'Late profile metadata completion preserves growth and mint updates; evolution trees are already persisted',
    ('PokemonNameLocalizationTests.swift','testSelectedLanguageThenEnglishWithCurrentAndLegacyCodes'): 'Metadata language resolution is tested independently; embedded Shark retains its host interface language',
    ('PokemonFormTests.swift','testSavesWrittenBeforeGenericFormsKeepTheirUnownLetters'): 'Legacy Rust serde aliases preserve Unown letters; Swift save envelopes are not accepted as Rust saves',
    ('UsageStoreTests.swift','testWeekMonthPersistAcrossRefreshWhenEnrichmentSkips'): 'Atomic Rust reports retain the previous successful history on failure rather than using upstream two-phase enrichment',
})
adaptations.update({
    **{('OutsideClickMonitorTests.swift',name): 'Native GPUI overlay subscriptions are window-owned: repeated open reuses focus, close is idempotent and reopening creates a fresh scope; a nil global registration token cannot be represented' for name in ('testStartTwiceKeepsFirstTokenAndDoesNotRegisterAgain','testStopWhenIdleDoesNotRemove','testStartAfterStopRegistersAgain','testNilRegisterDoesNotOccupyTheSlot')},
    **{('DexEntryLinkCursorTests.swift',name): 'Native GPUI frame-owned button hitboxes replace global NSCursor push/pop; mounted regressions cover hover and removal without stale hitboxes, but headless tests do not assert the macOS system cursor image' for name in ('testHoverShowsHandAndLeavingRestoresArrow','testDisappearingWhileHoveredRestoresArrow','testDisappearingUnhoveredLeavesOtherViewsCursorAlone')},
    ('RepresentativeFooterLayoutTests.swift','testRepresentativeActionStaysIconWidthInEveryLocaleAndState'): 'The native representative button is a fixed 28px icon with an accessible label; eight-language label data is tested separately while the host interface remains English',
    ('SpriteSubjectTests.swift','testSingleFrameFallsBackToStatic'): 'A validated single-frame GIF is displayed as a static decoded frame without fetching a separate PNG',
})
adaptations.update({
    ('MonthDailyTrendTests.swift','testCaptionStaysOnOneLineInEveryLanguageAtWorstCaseNumbers'): 'Mounted captions use the Shark English interface; the shared date and number formatters are separately exercised across all eight upstream locales',
    ('UsageRecapTests.swift','testTheCardRendersInEveryLanguageAndScope'): 'Mounted cards cover all scopes in the Shark English interface; eight-language recap label and date contracts are separately tested',
    **{('ZeroUsageHomeRenderingTests.swift',name): 'The accounting summary and history chart are embedded in Shark Usage rather than Pokémon Home; German cost-format data is tested separately from the English host interface' for name in ('testQuietDayKeepsHistoricalGraphVisible','testEmptyHistoryStillShowsZeroPeriodSummaries','testRecapLabelKeepsPeriodCostsOnOneLine','testUnavailablePeriodCostsKeepGermanHomeWithinPopover')},
    ('UsageStoreTests.swift','testConcurrentRefreshIsCoalescedNotDropped'): 'Shark shares one in-flight request and retries invalidated generations; it does not queue an arbitrary extra same-generation refresh',
    **{('UsageStoreTests.swift',name): 'Shark retains its existing remaining-percentage presentation; no used/remaining display preference is introduced' for name in ('testLimitDisplayPercentModes','testMenuBarLimitRemainingModeInvertsPercents')},
    **{('LimitProgressRenderingTests.swift',name): 'Native progress fill and elapsed markers are tested in Shark remaining mode; the standalone used/remaining preference is not introduced' for name in ('testRemainingModeReversesRenderedFillAndClampsExhaustedQuota','testPaceMarkerRendersAtElapsedPositionAndMirrorsInRemainingMode','testPaceMarkerStaysCloseToPaintedTrackHeight')},
})
adaptations.update({
    ('ModelLogicTests.swift','testFetchedLanguageCodesMatchAppLanguageAPICodesWithoutDuplicates'): 'Rust preserves language entries from REST metadata and checks shared language codes rather than generating the upstream GraphQL name-language allowlist',
    **{('PokemonNameLocalizationTests.swift',name): 'Mounted metadata labels use the Shark English interface; multi-language lookup, offline snapshots and fallback contracts are tested separately in the typed resource client' for name in ('testMountedLabelLoadsTranslationsAndReopensWithoutEnglishFrame','testLoadingNamesNeverShowsEnglishAndReopeningUsesSnapshot','testFailedNameLoadFallsBackAndRetryRestoresTranslation','testRenderedNamesResolveLanguageAndEnglishFallbackWithoutChangingKeys')},
})
adaptations[('CollectionReadabilityRenderingTests.swift','testGenerateCollectionReadabilityEvidence')] = 'Native GPUI visibility checks and Metal captures cover long English names, four rarities and paging; Shark retains its interface language instead of using SwiftUI/Vision OCR over eight whole-app locales'
markers = {}
for file in sorted((repo / 'src').rglob('*.rs')):
    for number, line in enumerate(file.read_text().splitlines(), 1):
        match = re.search(r'// upstream: (\w+\.swift)::(test\w+)\s*$', line)
        if match:
            key = match.groups()
            markers.setdefault(key, []).append(f'{file.relative_to(repo)}:{number}')
rows = []
seen = set()
for file in sorted((upstream / 'Tests' / 'PokeTokenBarTests').glob('*.swift')):
    text = file.read_text()
    for match in re.finditer(r'\bfunc\s+(test\w+)\s*\(', text):
        name = match.group(1)
        key = (file.name, name)
        if key in seen:
            raise SystemExit(f'Duplicate upstream identifier: {key}; use class-qualified markers')
        seen.add(key)
        line = text.count('\n', 0, match.start()) + 1
        if key in markers:
            status, reason = ('adapted', adaptations[key]) if key in adaptations else ('translated', 'Rust regression present; execute tests and review assertion equivalence separately')
        elif key in excluded_cases or file.name.startswith(other_providers):
            status, reason = 'out-of-scope', 'User limited provider support to Claude Code and Codex'
        elif file.name == 'PerformanceTests.swift' and name in {'testPanelGrowsForBubbleWithoutChangingPetOrigin', 'testClickThresholdDistinguishesClickFromDrag', 'testHoverTooltipBuilder', 'testHoverCalloutColorsFollowLightAndDarkAppearance', 'testLocalizedAlertBubbleFitsDefaultPanel', 'testThreeLineBodyThatFitsHeadroomWouldTruncate', 'testTwoLineBodyDoesNotTruncate', 'testFourLineBodyExceedsHeadroomAndWouldTruncate', 'testUnclampedBubbleTextWidthCanExceedContentColumn'}:
            status, reason = 'retained-shark-host', 'Standalone floating desktop pet panel is replaced by the requested Shark navigation footer'
        elif file.name in retained_host:
            status, reason = 'retained-shark-host', retained_host[file.name]
        elif key in retained_host_cases:
            status, reason = 'retained-shark-host', 'Standalone menu-bar, floating-pet, language-selection, status-page polling, OS notifications, warning preferences, diagnostic-mail or display-monitor preferences are replaced by Shark host lifecycle, interface language and remaining-usage presentation'
        elif file.name == 'StatusItemMenuTests.swift' and name != 'testSummaryOmitsCostWhenNoProviderReportsOne':
            status, reason = 'retained-shark-host', 'Shark sidebar and native settings replace standalone NSStatusItem menu and floating desktop pet controls'
        elif file.name in retained_quota or key in retained_quota_cases:
            status, reason = 'retained-shark-fetcher', 'User explicitly retained Shark remaining-usage fetching; upstream auth/quota mechanism is not ported'
        else:
            status, reason = 'pending', 'No mapped Rust regression yet'
        rows.append(dict(upstream_file=f'Tests/PokeTokenBarTests/{file.name}', upstream_test=name, upstream_line=line, status=status, rust_tests='; '.join(markers.get(key, [])), reason=reason))
unknown = set(markers) - seen
if unknown:
    raise SystemExit(f'Unknown upstream markers: {sorted(unknown)}')
path = repo / 'docs' / 'poketokenbar-test-migration.csv'
with path.open('w', newline='') as handle:
    writer = csv.DictWriter(handle, fieldnames=list(rows[0]), lineterminator='\n')
    writer.writeheader()
    writer.writerows(rows)
counts = Counter(row['status'] for row in rows)
summary = '\n'.join(f'| {name} | {counts[name]} |' for name in ('translated', 'adapted', 'out-of-scope', 'retained-shark-fetcher', 'retained-shark-host', 'pending'))
(repo / 'docs' / 'poketokenbar-test-migration.md').write_text(f'''# PokeTokenBar test migration

Upstream: https://github.com/chattymin/PokeTokenBar/tree/{PIN}

The [case inventory](poketokenbar-test-migration.csv) records **every one of the {len(rows):,} upstream XCTest methods**, with its original source location and a Rust test location where translated. Every source case has an explicit disposition. Host/provider exclusions are not claimed as translated tests; the executed QA results are recorded separately.

| Status | Cases |
| --- | ---: |
{summary}

`adapted` identifies an intentional Rust contract difference, with the exact rationale in the case inventory. `translated` means the case has a Rust regression carrying an upstream marker. It does not itself establish that the test passes or that every assertion has equivalent coverage. Run the Rust suite and review each translation. `pending` cases are unfinished work, not skipped or passing tests.

The user narrowed coding-tool support to Claude Code and Codex. Tests exclusive to other providers are `out-of-scope`. The user also explicitly retained Shark's existing remaining-usage fetching because it works more reliably. Tests specific to the upstream quota/authentication architecture are `retained-shark-fetcher`; the corresponding Shark implementation and existing Rust regressions remain authoritative. `retained-shark-host` records standalone updater, process lifecycle, menu-bar and language-interface tests whose host is Shark. These are explicitly not claimed as Rust translations. Shared parsing, companion, persistence and embedded UI tests remain pending until individually translated; they are not silently excluded.

Regenerate with:

```sh
python3 scripts/poketokenbar-test-inventory.py /path/to/PokeTokenBar
```

Rust tests use fixtures and deterministic seeds and do not access the user's accounts. Production uses Shark's provider adapters and runtime PokéAPI resources.
''')
print(f'{len(rows)} upstream tests: {dict(counts)}')
