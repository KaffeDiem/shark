//! Display text shared by the companion and its usage surfaces.
pub fn compact(value: u64) -> String {
    if value < 1_000 {
        return value.to_string();
    }
    let (scale, digits, suffix) = if value < 999_950 {
        (1_000., 1, "K")
    } else if value < 999_950_000 {
        (1_000_000., 1, "M")
    } else {
        (1_000_000_000., 2, "B")
    };
    format!("{}{suffix}", trim(value as f64 / scale, digits))
}
fn trim(value: f64, digits: usize) -> String {
    format!("{value:.digits$}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}
pub fn compact_cost(value: f64) -> String {
    let tenths = format!("{value:.1}");
    if tenths.parse::<f64>().is_ok_and(|n| n < 100.) {
        return format!("${tenths}");
    }
    let whole = format!("{value:.0}");
    if whole.parse::<f64>().is_ok_and(|n| n < 10_000.) {
        format!("${whole}")
    } else {
        format!("${:.1}K", value / 1000.)
    }
}

/// Groups digits with the given separator; independent of the machine's locale.
pub fn grouped(value: u64, separator: char) -> String {
    let digits = value.to_string();
    let mut result = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            result.push(separator)
        }
        result.push(c)
    }
    result
}

pub fn progress_detail(used: u64, total: u64) -> String {
    let used = used.min(total);
    let percent = if total == 0 {
        0
    } else {
        (used as u128 * 100 / total as u128) as u64
    };
    format!("{} / {} · {percent}%", compact(used), compact(total))
}

#[cfg(test)]
#[path = "tests/format_extra.rs"]
mod tests;
