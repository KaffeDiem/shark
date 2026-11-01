use super::*;
#[test]
fn progress_detail_clamps_and_floors_without_overflow() {
    for (used, total, text) in [
        (37200, 50000, "37.2K / 50K · 74%"),
        (60000, 50000, "50K / 50K · 100%"),
        (0, 0, "0 / 0 · 0%"),
        (999, 1000, "999 / 1K · 99%"),
    ] {
        assert_eq!(progress_detail(used, total), text);
    }
    assert!(progress_detail(u64::MAX, u64::MAX).ends_with("100%"));
    assert!(progress_detail(u64::MAX - 1, u64::MAX).ends_with("99%"));
}
