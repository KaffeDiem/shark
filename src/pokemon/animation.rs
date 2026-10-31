//! Persistent companion animation trades frame density for fewer idle wakeups, not slower playback.
use serde::{Deserialize, Serialize};
use std::time::Duration;
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Quality {
    #[default]
    PowerSaver,
    Balanced,
    Smooth,
}
impl<'de> Deserialize<'de> for Quality {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(match value.as_str() {
            Some("balanced") => Self::Balanced,
            Some("smooth") => Self::Smooth,
            _ => Self::PowerSaver,
        })
    }
}
impl Quality {
    pub const ALL: [Self; 3] = [Self::PowerSaver, Self::Balanced, Self::Smooth];
    pub fn name(self) -> &'static str {
        match self {
            Self::PowerSaver => "Power saver",
            Self::Balanced => "Balanced",
            Self::Smooth => "Smooth",
        }
    }
    pub fn frame_floor(self) -> Duration {
        Duration::from_millis(match self {
            Self::PowerSaver => 400,
            Self::Balanced => 200,
            Self::Smooth => 100,
        })
    }
    pub fn effective_floor(self, low_power: bool) -> Duration {
        if low_power {
            self.frame_floor().max(Self::PowerSaver.frame_floor())
        } else {
            self.frame_floor()
        }
    }
}
/// Each output keeps the first image in a group and its combined delay. A short tail merges
/// into the preceding group, conserving the loop duration exactly (including fractional delays).
pub fn cap_frames(delays: &[Duration], floor: Duration) -> Vec<(usize, Duration)> {
    if floor.is_zero() || delays.len() < 2 {
        return delays.iter().copied().enumerate().collect();
    }
    let mut out = vec![];
    let mut first = 0;
    let mut acc = Duration::ZERO;
    for (i, delay) in delays.iter().enumerate() {
        if acc.is_zero() {
            first = i;
        }
        acc = acc.saturating_add(*delay);
        if acc >= floor {
            out.push((first, acc));
            acc = Duration::ZERO;
        }
    }
    if !acc.is_zero() {
        if let Some((_, last)) = out.last_mut() {
            *last = last.saturating_add(acc);
        } else {
            out.push((first, acc));
        }
    }
    out
}
/// Select the frame on an absolute loop timeline. Late wakes skip to the current phase instead
/// of stretching every later frame by the time the UI thread was busy.
pub fn frame_at(delays: &[Duration], elapsed: Duration) -> (usize, Duration) {
    let total = delays.iter().copied().sum::<Duration>().as_nanos();
    if total == 0 {
        return (0, Duration::from_millis(100));
    }
    let mut phase = elapsed.as_nanos() % total;
    for (index, delay) in delays.iter().enumerate() {
        let duration = delay.as_nanos();
        if phase < duration {
            return (
                index,
                Duration::from_nanos((duration - phase).min(u64::MAX as u128) as u64),
            );
        }
        phase = phase.saturating_sub(duration);
    }
    (
        0,
        delays
            .first()
            .copied()
            .unwrap_or(Duration::from_millis(100)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frames(n: usize, ms: u64) -> Vec<Duration> {
        vec![Duration::from_millis(ms); n]
    }
    #[test]
    fn every_preset_caps_without_slowing() {
        for q in Quality::ALL {
            let capped = cap_frames(&frames(20, 100), q.frame_floor());
            assert!(q.frame_floor() > Duration::ZERO);
            assert!(capped.iter().all(|(_, d)| *d >= q.frame_floor()));
            assert_eq!(
                capped.iter().map(|(_, d)| *d).sum::<Duration>(),
                Duration::from_secs(2)
            );
        }
        assert!(Quality::PowerSaver.frame_floor() > Quality::Balanced.frame_floor());
        assert!(Quality::Balanced.frame_floor() > Quality::Smooth.frame_floor());
    }
    #[test]
    fn transient_is_the_only_native_rate() {
        let native = frames(55, 50);
        let output = cap_frames(&native, Duration::ZERO);
        assert_eq!(output.iter().map(|(_, d)| *d).collect::<Vec<_>>(), native);
        assert!(Quality::ALL.iter().all(|q| !q.frame_floor().is_zero()));
    }
    #[test]
    fn cap_conserves_long_and_slow_loops() {
        for floor in [200, 400] {
            let capped = cap_frames(&frames(55, 50), Duration::from_millis(floor));
            assert_eq!(
                capped.iter().map(|(_, d)| *d).sum::<Duration>(),
                Duration::from_millis(2750)
            );
            assert!(capped.len() > 1);
            assert!(capped.len() as f64 / 2.75 <= 1000. / floor as f64);
        }
        assert_eq!(
            cap_frames(&frames(4, 600), Duration::from_millis(400)).len(),
            4
        );
    }
    #[test]
    fn low_power_is_derived_and_restores_original_choice() {
        for q in Quality::ALL {
            assert!(q.effective_floor(true) >= Quality::PowerSaver.frame_floor());
            assert!(q.effective_floor(true) >= q.frame_floor());
            assert_eq!(q.effective_floor(false), q.frame_floor());
        }
        assert_ne!(
            Quality::Smooth.effective_floor(true),
            Quality::Smooth.effective_floor(false)
        );
        assert_eq!(
            Quality::PowerSaver.effective_floor(true),
            Quality::PowerSaver.effective_floor(false)
        );
    }
    #[test]
    fn late_wakes_catch_up_without_changing_loop_phase() {
        let delays = frames(4, 100);
        assert_eq!(
            frame_at(&delays, Duration::from_millis(110)),
            (1, Duration::from_millis(90))
        );
        assert_eq!(
            frame_at(&delays, Duration::from_millis(1210)),
            (0, Duration::from_millis(90))
        );
    }
    #[test]
    fn corrupt_and_future_saved_quality_defaults_safely() {
        for raw in [r#""future""#, "42", "null", "true", r#"{"unknown":1}"#] {
            assert_eq!(
                serde_json::from_str::<Quality>(raw).unwrap(),
                Quality::PowerSaver
            );
        }
        for quality in Quality::ALL {
            assert_eq!(
                serde_json::from_str::<Quality>(&serde_json::to_string(&quality).unwrap()).unwrap(),
                quality
            );
        }
    }
}
