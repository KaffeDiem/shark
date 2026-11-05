//! Display text for token counts.
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

#[cfg(test)]
mod tests;
