//! Daily token totals for the navigation heatmap, derived from the local-log report
//! ([`super::logs::Report::heatmap`]). Read-only; the aggregate stays in memory.
use objc2_foundation::{NSDate, NSTimeZone};

/// Weeks shown, and the days in them.
pub const WEEKS: i64 = 20;
pub const DAYS: usize = WEEKS as usize * 7;

/// Tokens per local day, oldest first; `first_day` is a [`local_day`] number.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Daily {
    pub first_day: i64,
    pub tokens: Vec<u64>,
}

/// Days since 1970-01-01 in the local time zone.
pub fn local_day(seconds: f64) -> i64 {
    let offset = NSTimeZone::localTimeZone()
        .secondsFromGMTForDate(&NSDate::dateWithTimeIntervalSince1970(seconds));
    ((seconds + offset as f64) / 86400.).floor() as i64
}
