use super::*;
#[test]
fn compact_promotes_rounded_units() {
    for (value, expected) in [
        (999, "999"),
        (1000, "1K"),
        (999949, "999.9K"),
        (999950, "1M"),
        (999999, "1M"),
        (1000000, "1M"),
        (999949999, "999.9M"),
        (999950000, "1B"),
        (999999999, "1B"),
        (1000000000, "1B"),
        (1240000000, "1.24B"),
    ] {
        assert_eq!(compact(value), expected)
    }
}
#[test]
fn compact_examples() {
    for (value, expected) in [
        (0, "0"),
        (987, "987"),
        (12345, "12.3K"),
        (190612940, "190.6M"),
        (1240000000, "1.24B"),
        (1000000, "1M"),
    ] {
        assert_eq!(compact(value), expected)
    }
}
