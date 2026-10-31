use super::*;
use crate::usage::{
    Allowance, Failure,
    pace::{self, Tier},
};

fn quota(remaining: Option<f64>) -> Snapshot {
    Snapshot {
        allowances: vec![Allowance {
            label: "5 hours".into(),
            remaining,
            resets_at: Some(2000.),
            window: Some(2000.),
        }],
        status: Status::Ready,
        ..Default::default()
    }
}

#[test]
fn compact_quota_preserves_low_unknown_and_refresh_states() {
    for (remaining, label) in [
        (Some(0.), "0%"),
        (Some(0.2), "<1%"),
        (Some(42.), "42%"),
        (Some(100.), "100%"),
        (None, "—"),
        (Some(f64::NAN), "—"),
    ] {
        assert_eq!(headline(&quota(remaining), 1000.).text, label);
    }
    let mut snapshot = quota(Some(42.));
    snapshot.begin();
    assert_eq!(headline(&snapshot, 1000.).text, "42%");
    assert!(
        quota_details("Claude", &snapshot, 1000.)
            .join(" ")
            .contains("checking")
    );
    snapshot.finish(Err(Failure::Temporary));
    assert_eq!(headline(&snapshot, 1000.).text, "—");
    assert_eq!(headline(&snapshot, 1000.).remaining, None);
    let details = quota_details("Claude", &snapshot, 1000.).join(" ");
    assert!(details.contains("42% left") && details.contains("outdated"));
    snapshot = Snapshot::default();
    assert_eq!(headline(&snapshot, 1000.).text, "—");
    snapshot.begin();
    assert_eq!(headline(&snapshot, 1000.).text, "…");
    snapshot.finish(Err(Failure::SignIn));
    assert_eq!(headline(&snapshot, 1000.).text, "—");
    assert_eq!(headline(&quota(Some(42.)), 2000.).remaining, None);
}

#[test]
fn headline_percentage_and_pace_use_the_same_tightest_active_window() {
    let mut snapshot = quota(Some(68.));
    snapshot.allowances.extend([
        Allowance {
            label: "Weekly".into(),
            remaining: Some(42.),
            resets_at: Some(2500.),
            window: Some(2000.),
        },
        Allowance {
            label: "Expired".into(),
            remaining: Some(0.),
            resets_at: Some(900.),
            window: Some(2000.),
        },
        Allowance {
            label: "Unknown".into(),
            remaining: None,
            resets_at: None,
            window: None,
        },
    ]);
    let value = headline(&snapshot, 1000.);
    assert_eq!(value.text, "42%");
    assert_eq!(value.remaining, Some(42.));
    assert_eq!(value.elapsed, Some(0.25));
    assert_eq!(
        pace::gauge(100. - value.remaining.unwrap(), value.elapsed, 80., 95.),
        Tier::WayOver
    );
}

#[test]
fn footer_pace_covers_all_colors_and_missing_window_fallback() {
    for (used, tier) in [
        (10., Tier::WayUnder),
        (30., Tier::Under),
        (50., Tier::OnPace),
        (60., Tier::SlightlyOver),
        (70., Tier::Over),
        (90., Tier::WayOver),
    ] {
        let snapshot = quota(Some(100. - used));
        let value = headline(&snapshot, 1000.);
        assert_eq!(
            pace::gauge(100. - value.remaining.unwrap(), value.elapsed, 80., 95.),
            tier
        );
        assert!(
            quota_details("Claude", &snapshot, 1000.)
                .join(" ")
                .contains(tier.label(crate::pokemon::format::Language::En))
        );
    }
    let mut snapshot = quota(Some(15.));
    snapshot.allowances[0].window = None;
    let value = headline(&snapshot, 1000.);
    assert_eq!(value.elapsed, None);
    assert_eq!(pace::gauge(85., value.elapsed, 80., 95.), Tier::Over);
}
