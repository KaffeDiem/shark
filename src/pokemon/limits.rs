//! Quota reset epoch comparison, independent of a provider's timestamp spelling.
pub fn parse_epoch(value: &str) -> Option<f64> {
    if let Ok(number) = value.parse::<f64>() {
        if number.is_finite() && number > 100_000_000. {
            return Some(if number > 1_000_000_000_000. {
                number / 1000.
            } else {
                number
            });
        }
    }
    let (date, time) = value.split_once('T')?;
    let fields = date
        .split('-')
        .map(str::parse::<i64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if fields.len() != 3 {
        return None;
    }
    let (mut year, month, day) = (fields[0], fields[1], fields[2]);
    // Bound the civil date before integer calendar arithmetic. These values can come
    // from an imported ledger, not just a provider's well-formed timestamp.
    if !(0..=9999).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !(1..=days_in_month).contains(&day) {
        return None;
    }
    let (clock, offset) = if let Some(clock) = time.strip_suffix('Z') {
        (clock, 0.)
    } else {
        let index = time.char_indices().find(|(_, c)| *c == '+' || *c == '-')?.0;
        let (clock, zone) = time.split_at(index);
        let sign = if zone.starts_with('-') { -1. } else { 1. };
        let (hours, minutes) = zone[1..].split_once(':')?;
        let hours = hours.parse::<u8>().ok()?;
        let minutes = minutes.parse::<u8>().ok()?;
        if hours > 23 || minutes > 59 {
            return None;
        }
        (
            clock,
            sign * (f64::from(hours) * 3600. + f64::from(minutes) * 60.),
        )
    };
    let parts = clock
        .split(':')
        .map(str::parse::<f64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if parts.len() != 3
        || parts.iter().any(|v| !v.is_finite() || *v < 0.)
        || parts[0] >= 24.
        || parts[1] >= 60.
        || parts[2] >= 60.
    {
        return None;
    }
    year -= i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let days = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468;
    Some(days as f64 * 86400. + parts[0] * 3600. + parts[1] * 60. + parts[2] - offset)
}
pub fn is_new_epoch(previous: &str, current: &str, weekly: bool) -> bool {
    if previous == current {
        return false;
    }
    match (parse_epoch(previous), parse_epoch(current)) {
        (Some(a), Some(b)) => b - a >= if weekly { 86400. } else { 1800. },
        _ => true,
    }
}
