use super::super::*;
use super::economy::line;
fn raised(difficulty: f64, forms: usize) -> Companion {
    let mut s = Companion::default();
    s.set_growth_difficulty(difficulty);
    s.egg_tokens = s.progress().1;
    s.hatch(line(forms));
    s
}
fn final_details(species: u16) -> profile::Details {
    profile::Details {
        species_id: species,
        types: vec![],
        gender_rate: -1,
        base_stats: Default::default(),
        abilities: vec![],
        moves: vec![profile::Move {
            name: format!("final-{species}"),
            learn_methods: vec![profile::LearnMethod {
                method: "level-up".into(),
                level: 100,
            }],
        }],
    }
}
#[test]
fn testEveryDifficultyAndRepeatHatchGraduatesAtLevel100() {
    for difficulty in [0.1, 1., 2.] {
        for forms in 1..=3 {
            let mut s = raised(difficulty, forms);
            for repeated in [false, true] {
                if repeated {
                    s.egg_tokens = s.progress().1;
                    s.hatch(line(forms));
                }
                assert_eq!(s.active.as_ref().unwrap().growth_boost, repeated);
                assert_eq!(
                    s.active.as_ref().unwrap().profile.as_ref().unwrap().level,
                    5
                );
                s.enrich_profiles(&final_details(forms as u16));
                for _ in 0..forms {
                    let (used, total) = s.progress();
                    s.add_growth(total - used);
                }
                assert!(s.active.is_none());
                let p = s.collection.last().unwrap().profile.as_ref().unwrap();
                assert_eq!(p.level, 100);
                assert_eq!(p.growth_tokens, 750_000_000);
                assert_eq!(
                    p.moves.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
                    [format!("final-{forms}")]
                );
            }
        }
    }
}
#[test]
fn testDifficultyChangesAndRestartNeverUndoEarnedLevels() {
    let mut s = raised(0.5, 1);
    s.add_growth(375_000_000);
    s.egg_tokens = s.progress().1;
    s.hatch(line(1));
    assert!(s.active.as_ref().unwrap().growth_boost);
    s.add_growth(93_750_000);
    let before = s.active.as_ref().unwrap().profile.clone().unwrap();
    assert_eq!(before.level, 52);
    s.set_growth_difficulty(2.);
    assert_eq!(
        s.active.as_ref().unwrap().profile.as_ref().unwrap().level,
        52
    );
    let restored: Companion = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
    let p = restored.active.unwrap().profile.unwrap();
    assert_eq!(p.instance_id, before.instance_id);
    assert_eq!(p.ivs, before.ivs);
    assert_eq!(p.level, 52);
    s.set_growth_difficulty(0.1);
    assert_eq!(s.progress().0 * 2, s.progress().1);
    assert_eq!(
        s.active.as_ref().unwrap().profile.as_ref().unwrap().level,
        52
    );
    let (used, total) = s.progress();
    s.add_growth(total - used);
    assert!(s.active.is_none());
    assert_eq!(
        s.collection.last().unwrap().profile.as_ref().unwrap().level,
        100
    );
}
#[test]
fn testImportOntoHarderDeviceKeepsEarnedIdentityAndLevel() {
    let mut source = raised(0.1, 1);
    source.add_growth(37_500_000);
    let original = source.active.as_ref().unwrap().profile.clone().unwrap();
    assert_eq!(original.level, 52);
    let bytes = storage::encode(&source, 0).unwrap();
    let mut target = Companion::default();
    target.set_growth_difficulty(2.);
    let mut imported = storage::rebase(
        storage::decode(&bytes).unwrap().state,
        &target,
        Default::default(),
    );
    assert_eq!(imported.growth_difficulty, 2.);
    let p = imported.active.as_ref().unwrap().profile.as_ref().unwrap();
    assert_eq!(p.instance_id, original.instance_id);
    assert_eq!(p.ivs, original.ivs);
    assert_eq!(p.level, 52);
    let (used, total) = imported.progress();
    imported.add_growth(total - used);
    assert_eq!(
        imported
            .collection
            .last()
            .unwrap()
            .profile
            .as_ref()
            .unwrap()
            .level,
        100
    );
}
#[test]
fn testRareCandyFinalizesGraduationProfile() {
    let mut s = raised(0.1, 3);
    s.inventory.candies = 1;
    s.enrich_profiles(&final_details(3));
    s.use_candy(1).unwrap();
    assert!(s.active.is_none());
    let p = s.collection.last().unwrap().profile.as_ref().unwrap();
    assert_eq!(p.level, 100);
    assert_eq!(
        p.moves.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
        ["final-3"]
    );
}
// The Rust save persists its validated evolution tree, so growth does not depend on a line download.
#[test]
fn testOfflineOverflowCatchesUpWhenLineLoads() {
    let mut s = raised(0.1, 3);
    s = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
    s.add_growth(75_000_000);
    assert!(s.active.is_none());
    assert_eq!(
        s.collection.last().unwrap().profile.as_ref().unwrap().level,
        100
    );
}
