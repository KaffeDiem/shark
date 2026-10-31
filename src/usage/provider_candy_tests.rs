use super::*;
use crate::pokemon::Companion;

fn snapshot(session: Option<f64>, weekly: Option<f64>) -> Snapshot {
    let mut snapshot = Snapshot::default();
    snapshot.finish(Ok([
        ("5 hours", session, 18_000.),
        ("Weekly", weekly, 604_800.),
    ]
    .into_iter()
    .map(|(label, remaining, window)| Allowance {
        label: label.into(),
        remaining,
        resets_at: Some(1_800_000_000.),
        window: Some(window),
    })
    .collect()));
    snapshot
}
fn observe(state: &mut Companion, provider: &str, snapshot: &Snapshot) -> u64 {
    eligible_candy_windows(provider, snapshot)
        .into_iter()
        .map(|w| state.observe_limit(&w.key, w.epoch.as_deref(), w.utilization, w.weekly, true))
        .sum()
}

// upstream: RareCandyTests.swift::testEligibleWindowsClaudeSessionAndWeekly
// upstream: RareCandyTests.swift::testEligibleWindowNamesForNotificationBody
#[test]
fn headline_windows_have_distinct_keys_and_readable_names() {
    for provider in ["claude", "codex"] {
        let windows = eligible_candy_windows(provider, &snapshot(Some(0.), Some(0.)));
        assert_eq!(windows.len(), 2);
        assert!(!windows[0].weekly);
        assert!(windows[1].weekly);
        assert_eq!(windows[0].key, format!("{provider}:session"));
        assert_eq!(windows[1].key, format!("{provider}:weekly"));
        let name = builtins().get(provider).unwrap().name();
        assert_eq!(windows[0].name, format!("{name} 5-hour limit"));
        assert_eq!(windows[1].name, format!("{name} weekly limit"));
        assert_eq!(windows[0].utilization, 100.);
    }
}

// upstream: RareCandyTests.swift::testEligibleWindowsExcludeOpusSonnet
// upstream: RareCandyTests.swift::testEligibleWindowsCodexClassificationExcludesSpend
#[test]
fn model_and_spend_caps_do_not_duplicate_headline_rewards() {
    let mut snapshot = snapshot(Some(0.), Some(0.));
    for label in ["Opus", "Sonnet", "Individual spend", "Model · Weekly"] {
        snapshot.allowances.push(Allowance {
            label: label.into(),
            remaining: Some(0.),
            resets_at: None,
            window: Some(604_800.),
        });
    }
    assert_eq!(eligible_candy_windows("claude", &snapshot).len(), 2);
    assert_eq!(eligible_candy_windows("codex", &snapshot).len(), 2);
}

// upstream: RareCandyTests.swift::testLimitsReadyReflectsLoad
#[test]
fn reward_windows_wait_for_fresh_successful_limits() {
    let mut snapshot = snapshot(Some(0.), Some(0.));
    for status in [
        Status::Idle,
        Status::Loading,
        Status::Failed(Failure::Temporary),
        Status::Failed(Failure::SignIn),
    ] {
        snapshot.status = status;
        assert!(eligible_candy_windows("claude", &snapshot).is_empty());
    }
    snapshot.status = Status::Ready;
    snapshot.outdated = true;
    assert!(eligible_candy_windows("claude", &snapshot).is_empty());
    snapshot.outdated = false;
    assert_eq!(eligible_candy_windows("claude", &snapshot).len(), 2);
}

// upstream: RareCandyTests.swift::testClaudeSessionLimitGrantsOneCandy
// upstream: RareCandyTests.swift::testClaudeWeeklyLimitGrantsFive
// upstream: RareCandyTests.swift::testSessionAndWeeklyTogetherGrantSix
// upstream: RareCandyTests.swift::testCodexPrimaryLimitGrantsOne
// upstream: RareCandyTests.swift::testRepeatedRefreshGrantsOnce
#[test]
fn headline_limits_award_one_or_five_once_per_window() {
    for (provider, session, weekly, expected) in [
        ("claude", Some(0.), None, 1),
        ("claude", None, Some(0.), 5),
        ("claude", Some(0.), Some(0.), 6),
        ("codex", Some(0.), None, 1),
    ] {
        let mut state = Companion::default();
        observe(&mut state, provider, &snapshot(Some(50.), Some(50.)));
        assert_eq!(
            observe(&mut state, provider, &snapshot(session, weekly)),
            expected
        );
        for _ in 0..5 {
            assert_eq!(observe(&mut state, provider, &snapshot(session, weekly)), 0);
        }
        assert_eq!(state.inventory.candies, expected);
    }
}

// upstream: RareCandyTests.swift::testJustUnder100NoGrant
// upstream: RareCandyTests.swift::testCandyThresholdSeparateFromAlertThreshold
#[test]
fn warning_percentages_never_award_candy() {
    let mut state = Companion::default();
    observe(&mut state, "claude", &snapshot(Some(50.), None));
    for remaining in [3., 0.1] {
        assert_eq!(
            observe(&mut state, "claude", &snapshot(Some(remaining), None)),
            0
        );
    }
    assert_eq!(state.inventory.candies, 0);
}

// upstream: RareCandyTests.swift::testFirstRunAt100SeedsNoGrant
// upstream: RareCandyTests.swift::testStaggeredProviderSeedRetroactiveGrant
#[test]
fn newly_seen_provider_never_awards_preinstallation_candy() {
    // Intentional correction of upstream's staggered-provider bug: baseline each window.
    let mut state = Companion::default();
    for provider in ["claude", "codex"] {
        for _ in 0..2 {
            assert_eq!(
                observe(&mut state, provider, &snapshot(Some(0.), Some(0.))),
                0
            );
        }
    }
    assert_eq!(state.inventory.candies, 0);
    assert_eq!(state.limit_baselines.len(), 4);
}

// upstream: RareCandyTests.swift::testEligibleWindowsSkipNilUtilization
#[test]
fn unknown_and_nonfinite_percentages_are_not_reward_signals() {
    for remaining in [None, Some(f64::NAN), Some(f64::INFINITY)] {
        assert!(eligible_candy_windows("claude", &snapshot(remaining, remaining)).is_empty());
    }
}

#[test]
fn corrupt_future_provider_percentages_are_not_clamped_into_rewards() {
    for remaining in [-1., 101., f64::NEG_INFINITY] {
        assert!(
            eligible_candy_windows("future-tool", &snapshot(Some(remaining), Some(remaining)))
                .is_empty()
        );
    }
}

// upstream: RareCandyTests.swift::testNoSeedWhenLimitsNotReady
#[test]
fn unloaded_limits_do_not_establish_reward_baselines() {
    let mut state = Companion::default();
    let mut limits = snapshot(Some(0.), Some(0.));
    for status in [
        Status::Idle,
        Status::Loading,
        Status::Failed(Failure::Temporary),
    ] {
        limits.status = status;
        assert_eq!(observe(&mut state, "claude", &limits), 0);
        assert!(state.limit_baselines.is_empty());
        assert_eq!(state.inventory.candies, 0);
    }
    limits.status = Status::Ready;
    assert_eq!(observe(&mut state, "claude", &limits), 0);
    assert_eq!(state.limit_baselines.len(), 2);
}
