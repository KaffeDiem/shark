use super::*;
// upstream: KoreanParticleTests.swift::testSubjectFollowsFinalConsonant
// upstream: KoreanParticleTests.swift::testObjectFollowsFinalConsonant
// upstream: KoreanParticleTests.swift::testDirectionTreatsRieulLikeNoFinalConsonant
// upstream: KoreanParticleTests.swift::testNonHangulEndingKeepsBothForms
#[test]
fn korean_particles_follow_final_syllable_and_preserve_unknown_suffix() {
    use KoreanParticle::*;
    for (particle, name, expected) in [
        (Subject, "유니란", "유니란이"),
        (Subject, "피카츄", "피카츄가"),
        (Object, "꼬부기", "꼬부기를"),
        (Object, "리자몽", "리자몽을"),
        (Direction, "란쿨루스", "란쿨루스로"),
        (Direction, "듀란", "듀란으로"),
        (Direction, "희귀 알", "희귀 알로"),
        (Direction, "레트라", "레트라로"),
        (Subject, "안농 [A]", "안농 [A]이(가)"),
        (Object, "#123", "#123을(를)"),
        (Direction, "Pikachu", "Pikachu(으)로"),
        (Subject, "", "이(가)"),
    ] {
        assert_eq!(particle.attach(name), expected);
    }
}
// upstream: KoreanParticleTests.swift::testKoreanCopyUsesTheMatchingParticle
// upstream: KoreanParticleTests.swift::testOtherLanguagesAreUntouched
#[test]
fn pokemon_event_copy_uses_matching_particles_only_in_korean() {
    assert_eq!(Language::Ko.hatch("유니란"), "알에서 유니란이 나왔어요!");
    assert_eq!(
        Language::Ko.shiny_hatch("뚜벅쵸", 48),
        "이로치 뚜벅쵸가 태어났어요! (1/48)"
    );
    assert_eq!(Language::Ko.evolved("듀란"), "듀란으로 진화했어요!");
    assert_eq!(Language::Ko.evolved("란쿨루스"), "란쿨루스로 진화했어요!");
    assert_eq!(
        Language::Ko.egg_confirm("레트라", "희귀 알"),
        "레트라를 놓아주고 희귀 알로 바꿀까요?"
    );
    assert_eq!(
        Language::En.hatch("Unfezant"),
        "Unfezant hatched from the egg!"
    );
    assert_eq!(
        Language::Ja.evolved("デスカーン"),
        "デスカーン に進化しました！"
    );
    for lang in Language::ALL {
        assert!(lang.hatch("SPECIES").contains("SPECIES"));
        assert!(lang.evolved("SPECIES").contains("SPECIES"));
        assert!(lang.egg_confirm("SPECIES", "EGG").contains("EGG"));
    }
}
// upstream: ProgressDetailTests.swift::testNormalProgressFormatsCompactCountsAndFlooredPercent
// upstream: ProgressDetailTests.swift::testUsedOverTotalIsClampedToTotal
// upstream: ProgressDetailTests.swift::testZeroTotalDoesNotCrashAndReportsZeroPercent
// upstream: ProgressDetailTests.swift::testPercentFloorsRatherThanRounds
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
