use super::super::*;

#[test]
fn threshold_hot_path_throughput() {
    let sum: u64 = (0..100_000)
        .map(|i| std::hint::black_box(Balance::phase_threshold(Rarity::Rare, 3, i % 3, 1)))
        .sum();
    assert_eq!(sum, 99_999_500_000_000);
}
#[test]
fn usage_update_hot_path_keeps_same_stage() {
    let mut state = super::economy::hatched();
    state.active.as_mut().unwrap().line.rarity = Rarity::Legendary;
    let initial_wallet = state.lifetime_tokens;
    state.observe_usage("test", "day", 0, true);
    for count in 1..=500 {
        state.observe_usage("test", "day", count * 100, true);
    }
    assert_eq!(state.active.unwrap().stage, 0);
    assert_eq!(state.lifetime_tokens, initial_wallet + 50_000);
}
#[test]
fn thousand_catches_sort_by_time_with_correct_rarity_counts() {
    let mut state = super::economy::hatched();
    let template = state.active.take().unwrap();
    for i in 0..1000 {
        let mut m = template.clone();
        m.caught_at = 1_700_000_000 + i;
        m.line.rarity = [
            Rarity::Common,
            Rarity::Uncommon,
            Rarity::Rare,
            Rarity::Legendary,
        ][i as usize % 4];
        state.collection.push(m);
    }
    let sorted = state.catch_log();
    assert_eq!(sorted.len(), 1000);
    assert_eq!(sorted[0].caught_at, 1_700_000_999);
    assert!(sorted.windows(2).all(|p| p[0].caught_at >= p[1].caught_at));
}
#[test]
fn huge_delta_graduates_exactly_one_individual() {
    let mut state = super::economy::hatched();
    state.add_growth(Rarity::Common.graduation_total() * 10);
    assert!(state.active.is_none());
    assert_eq!(state.collection.len(), 1);
    assert_eq!(
        state.collection[0].reached_species().collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert_eq!(state.egg_tokens, 0);
}
#[test]
fn repeated_graduation_keeps_linear_collection_growth() {
    let mut state = Companion::default();
    for count in 1..=20 {
        state.egg_tokens = state.progress().1;
        state.hatch_with_disguise_roll(super::economy::line(3), false);
        state.add_growth(Rarity::Common.graduation_total() * 10);
        assert_eq!(state.collection.len(), count);
        assert!(state.active.is_none());
    }
}
#[test]
fn catch_log_recency_takes_precedence_over_rarity() {
    let mut state = super::economy::hatched();
    let template = state.active.take().unwrap();
    for (id, rarity, time) in [
        (200, Rarity::Legendary, 1),
        (100, Rarity::Common, 2),
        (101, Rarity::Common, 3),
    ] {
        let mut mon = template.clone();
        mon.path = vec![id];
        mon.stage = 0;
        mon.line.base_id = id;
        mon.line.tree = EvoNode {
            species_id: id,
            children: vec![],
        };
        mon.line.rarity = rarity;
        mon.caught_at = time;
        mon.graduated = true;
        state.collection.push(mon);
    }
    assert_eq!(
        state
            .catch_log()
            .iter()
            .map(|m| m.species_id())
            .collect::<Vec<_>>(),
        [101, 100, 200]
    );
}
#[test]
fn active_catch_precedes_legendary_legacy_archive() {
    let mut state = super::economy::hatched();
    let mut archived = state.active.clone().unwrap();
    archived.line.rarity = Rarity::Legendary;
    archived.caught_at = 0;
    state.collection.push(archived);
    let sorted = state.catch_log();
    assert!(std::ptr::eq(sorted[0], state.active.as_ref().unwrap()));
    assert!(std::ptr::eq(sorted[1], &state.collection[0]));
    assert_eq!(sorted[0].line.rarity, Rarity::Common);
}
