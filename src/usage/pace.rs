//! Quota pace presentation. Allowance fetching and authentication remain Shark's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    WayUnder,
    Under,
    OnPace,
    SlightlyOver,
    Over,
    WayOver,
}
impl Tier {
    #[cfg(test)]
    pub const ALL: [Self; 6] = [
        Self::WayUnder,
        Self::Under,
        Self::OnPace,
        Self::SlightlyOver,
        Self::Over,
        Self::WayOver,
    ];
    pub fn label(self) -> &'static str {
        [
            "Well below pace",
            "Below pace",
            "On pace",
            "Slightly above pace",
            "Above pace",
            "Well above pace",
        ][self as usize]
    }
}
pub fn elapsed(reset: Option<f64>, span: Option<f64>, now: f64) -> Option<f64> {
    let (reset, span) = (reset?, span?);
    let left = reset - now;
    (reset.is_finite()
        && span.is_finite()
        && now.is_finite()
        && span > 0.
        && left >= 0.
        && left <= span)
        .then(|| 1. - left / span)
}
pub fn rounded_delta(utilization: f64, pace: f64) -> i64 {
    (utilization - pace * 100.).round() as i64
}
pub fn tier(utilization: f64, pace: Option<f64>, critical: f64) -> Option<Tier> {
    let pace = pace.filter(|p| p.is_finite() && *p >= 0.1 && *p <= 1.)?;
    if !utilization.is_finite() {
        return None;
    }
    if utilization >= critical {
        return Some(Tier::WayOver);
    }
    Some(match rounded_delta(utilization, pace) {
        ..=-26 => Tier::WayUnder,
        ..=-11 => Tier::Under,
        ..=4 => Tier::OnPace,
        ..=14 => Tier::SlightlyOver,
        ..=29 => Tier::Over,
        _ => Tier::WayOver,
    })
}
pub fn gauge(utilization: f64, pace: Option<f64>, warning: f64, critical: f64) -> Tier {
    tier(utilization, pace, critical).unwrap_or(if utilization >= critical {
        Tier::WayOver
    } else if utilization >= warning {
        Tier::Over
    } else {
        Tier::OnPace
    })
}
pub fn marker(pace: Option<f64>, remaining: bool) -> Option<f64> {
    pace.filter(|p| p.is_finite()).map(|p| {
        if remaining {
            1. - p.clamp(0., 1.)
        } else {
            p.clamp(0., 1.)
        }
    })
}
pub fn delta_text(delta: i64) -> Option<String> {
    if delta == 0 {
        return None;
    }
    let suffix = if delta > 0 { "pts over" } else { "pts under" };
    Some(format!("{} {suffix}", delta.unsigned_abs()))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn boundaries_and_rounded_deltas() {
        use Tier::*;
        for (n, t) in [
            (0., WayUnder),
            (24., WayUnder),
            (25., Under),
            (39., Under),
            (40., OnPace),
            (54., OnPace),
            (55., SlightlyOver),
            (64., SlightlyOver),
            (65., Over),
            (79., Over),
            (80., WayOver),
        ] {
            assert_eq!(tier(n, Some(0.5), 95.), Some(t));
        }
        assert_eq!(tier(54.6, Some(0.5), 95.), Some(SlightlyOver));
        assert_eq!(tier(54.4, Some(0.5), 95.), Some(OnPace));
        assert_eq!(rounded_delta(54.6, 0.5), 5);
        assert_eq!(rounded_delta(38.5, 0.5), -12);
    }
    #[test]
    fn critical_missing_and_early_window() {
        assert_eq!(tier(95., Some(0.99), 95.), Some(Tier::WayOver));
        assert_eq!(tier(94., Some(0.99), 95.), Some(Tier::OnPace));
        assert!(tier(50., None, 95.).is_none());
        assert!(tier(10., Some(0.02), 95.).is_none());
        assert!(tier(99., Some(0.099), 95.).is_none());
        assert_eq!(tier(10., Some(0.1), 95.), Some(Tier::OnPace));
        assert!(tier(f64::NAN, Some(0.5), 95.).is_none());
    }
    #[test]
    fn gauge_fallback_and_calm_tiers() {
        assert_eq!(gauge(20., Some(0.5), 80., 95.), Tier::WayUnder);
        assert_eq!(gauge(70., Some(0.5), 80., 95.), Tier::Over);
        for pace in [None, Some(0.05)] {
            for (n, t) in [
                (79., Tier::OnPace),
                (80., Tier::Over),
                (94., Tier::Over),
                (95., Tier::WayOver),
            ] {
                assert_eq!(gauge(n, pace, 80., 95.), t);
            }
        }
    }
    #[test]
    fn labels_preserve_distinction_and_unsigned_delta() {
        assert!(delta_text(0).is_none());
        assert_ne!(delta_text(-12), delta_text(12));
        assert!(!delta_text(-12).unwrap().contains("-12"));
        assert_eq!(
            Tier::ALL
                .map(Tier::label)
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            6
        );
        assert!(
            delta_text(i64::MIN)
                .unwrap()
                .contains("9223372036854775808")
        );
    }
    #[test]
    fn elapsed_windows_and_remaining_markers() {
        for (left, p) in [(18000., 0.), (9000., 0.5), (4500., 0.75), (0., 1.)] {
            assert_eq!(
                elapsed(Some(1800000000. + left), Some(18000.), 1800000000.),
                Some(p)
            );
        }
        for (left, span) in [
            (-1., 18000.),
            (18001., 18000.),
            (9000., 0.),
            (9000., -18000.),
        ] {
            assert!(elapsed(Some(1800000000. + left), Some(span), 1800000000.).is_none());
        }
        assert_eq!(marker(Some(0.25), false), Some(0.25));
        assert_eq!(marker(Some(0.25), true), Some(0.75));
        assert_eq!(marker(Some(1.), true), Some(0.));
        assert_eq!(marker(None, true), None);
        assert_eq!(marker(None, false), None);
    }
}
