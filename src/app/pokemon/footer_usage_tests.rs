use super::*;
use crate::usage::{Allowance, Failure};

fn quota(remaining: Option<f64>) -> Snapshot {
    Snapshot {
        allowances: vec![Allowance {
            label: "5 hours".into(),
            remaining,
            resets_at: Some(2000.),
            window: Some(18000.),
        }],
        status: Status::Ready,
        ..Default::default()
    }
}

#[test]
fn account_quotas_remain_visible_without_activity_in_both_layouts() {
    for rail in [false, true] {
        for (name, remaining) in [("Claude", 42.), ("Codex", 68.)] {
            let text = quota_lines(name, &quota(Some(remaining)), 1000., rail).join(" ");
            assert!(text.contains(name), "{text}");
            assert!(text.contains(&format!("{remaining}% left")), "{text}");
        }
    }
}

#[test]
fn quota_states_preserve_low_unknown_and_cached_allowances() {
    for rail in [false, true] {
        for (remaining, label) in [
            (Some(0.), "0% left"),
            (Some(0.2), "<1% left"),
            (None, "left unknown"),
        ] {
            assert!(
                quota_lines("Claude", &quota(remaining), 1000., rail)
                    .join(" ")
                    .contains(label)
            );
        }
        let mut snapshot = quota(Some(42.));
        snapshot.finish(Err(Failure::Temporary));
        let text = quota_lines("Claude", &snapshot, 1000., rail).join(" ");
        assert!(
            text.contains("42% left") && text.contains("outdated"),
            "{text}"
        );
        let mut snapshot = Snapshot::default();
        assert!(
            quota_lines("Codex", &snapshot, 1000., rail)
                .join(" ")
                .contains("not checked")
        );
        snapshot.begin();
        assert!(
            quota_lines("Codex", &snapshot, 1000., rail)
                .join(" ")
                .contains("checking")
        );
        snapshot.finish(Err(Failure::SignIn));
        assert!(
            quota_lines("Codex", &snapshot, 1000., rail)
                .join(" ")
                .contains("unavailable")
        );
        let text = quota_lines("Claude", &quota(Some(42.)), 2000., rail).join(" ");
        assert!(!text.contains("42%"), "expired quota: {text}");
    }
}

#[test]
fn compact_headline_uses_tightest_active_known_quota() {
    let mut snapshot = quota(Some(68.));
    snapshot.allowances.extend([
        Allowance {
            label: "Weekly".into(),
            remaining: Some(42.),
            resets_at: None,
            window: None,
        },
        Allowance {
            label: "Expired".into(),
            remaining: Some(0.),
            resets_at: Some(900.),
            window: None,
        },
        Allowance {
            label: "Unknown".into(),
            remaining: None,
            resets_at: None,
            window: None,
        },
    ]);
    assert_eq!(
        quota_lines("Claude", &snapshot, 1000., false),
        ["Claude 42% left"]
    );
    assert_eq!(
        quota_lines("Claude", &snapshot, 1000., true),
        ["Claude", "42% left"]
    );
}
