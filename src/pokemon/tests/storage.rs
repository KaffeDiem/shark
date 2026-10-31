use super::*;
use crate::pokemon::{Balance, EvoLine, EvoNode, Rarity};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "shark-pokemon-storage-{}-{}",
            std::process::id(),
            crate::pokemon::seed()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn snapshots(&self) -> Snapshots {
        Snapshots::new(self.0.join("snapshots"))
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn sample(tokens: u64) -> Companion {
    let mut state = Companion::default();
    state.random_seed = 42;
    state.egg_tokens = Balance::EGG_HATCH_THRESHOLD;
    state.hatch(EvoLine {
        base_id: 1,
        rarity: Rarity::Common,
        names: BTreeMap::new(),
        tree: EvoNode {
            species_id: 1,
            children: vec![EvoNode {
                species_id: 2,
                children: vec![EvoNode {
                    species_id: 3,
                    children: vec![],
                }],
            }],
        },
    });
    state.lifetime_tokens = tokens;
    state.inventory.candies = 3;
    state
}
fn value(s: &Companion) -> serde_json::Value {
    serde_json::to_value(s).unwrap()
}

#[test]
fn round_trip_preserves_progress() {
    let mut s = sample(5_000_000_000);
    s.spent_tokens = 250_000_000;
    s.add_growth(125_000_000);
    s.pin(Some(2));
    s.collection.push(s.active.clone().unwrap());
    let result = decode(&encode(&s, 123).unwrap()).unwrap();
    assert_eq!(result.format, FORMAT);
    assert_eq!(result.exported_at, 123);
    assert_eq!(value(&result.state), value(&s));
}

#[test]
fn repeat_growth_survives_transfer() {
    let mut s = sample(0);
    s.active.as_mut().unwrap().growth_boost = true;
    s.active.as_mut().unwrap().tokens = 12_000_000;
    let result = decode(&encode(&s, 0).unwrap()).unwrap().state;
    let m = result.active.unwrap();
    assert!(m.growth_boost);
    assert_eq!(m.tokens, 12_000_000);
    assert_eq!(m.threshold(1.), 62_500_000);
}

#[test]
fn foreign_json_rejected() {
    for raw in [
        b"{}".as_slice(),
        b"null",
        b"[]",
        br#"{"pokemon":{}}"#,
        b"invalid",
    ] {
        assert_eq!(decode(raw).unwrap_err(), Error::NotASave);
    }
    assert_eq!(
        decode(&serde_json::to_vec(&sample(0)).unwrap()).unwrap_err(),
        Error::NotASave
    );
}

#[test]
fn wrong_format_rejected() {
    let mut envelope: serde_json::Value =
        serde_json::from_slice(&encode(&sample(0), 0).unwrap()).unwrap();
    envelope["format"] = "another.app".into();
    assert_eq!(
        decode(&serde_json::to_vec(&envelope).unwrap()).unwrap_err(),
        Error::NotASave
    );
}

#[test]
fn future_schema_reported_before_body_decoding() {
    for mut envelope in [
        serde_json::from_slice::<serde_json::Value>(&encode(&sample(0), 0).unwrap()).unwrap(),
        serde_json::json!({"format": FORMAT}),
    ] {
        envelope["schema"] = 99.into();
        assert_eq!(
            decode(&serde_json::to_vec(&envelope).unwrap()).unwrap_err(),
            Error::NewerSchema(99)
        );
    }
}

#[test]
fn oversized_input_rejected_before_parsing() {
    assert_eq!(
        decode(&vec![0; MAX_FILE_BYTES + 1]).unwrap_err(),
        Error::TooLarge
    );
    let dir = Temp::new();
    let path = dir.0.join("huge.json");
    fs::File::create(&path)
        .unwrap()
        .set_len(MAX_FILE_BYTES as u64 + 1000)
        .unwrap();
    assert_eq!(read(&path).unwrap_err(), Error::TooLarge);
}

#[test]
fn imported_usage_rebased_to_local_checkpoint() {
    let mut imported = sample(3_000_000);
    imported.observe_usage("claude", "2026-08-03", 100_000, true);
    let local = Companion::default();
    let usage = BTreeMap::from([(
        "claude".into(),
        UsageBaseline {
            day: "2026-08-03".into(),
            total: 12_000,
        },
    )]);
    let mut result = rebase(imported, &local, usage);
    result.observe_usage("claude", "2026-08-03", 12_600, true);
    assert_eq!(result.lifetime_tokens, 3_000_600);
}

#[test]
fn no_current_usage_defers_install_baseline() {
    for current in [Companion::default(), sample(50_000)] {
        let mut imported = sample(20_000);
        imported.observe_usage("claude", "2026-08-02", 123, true);
        let mut result = rebase(imported, &current, BTreeMap::new());
        assert!(result.baselines.is_empty());
        result.observe_usage("claude", "2026-08-03", 500_000, true);
        assert_eq!(result.lifetime_tokens, 20_000);
        result.observe_usage("claude", "2026-08-03", 500_700, true);
        assert_eq!(result.lifetime_tokens, 20_700);
    }
}

#[test]
fn import_preserves_claimed_rewards_from_both_devices() {
    let mut local = sample(0);
    local.observe_limit("claude:5h", Some("0002"), 100., false, true);
    let mut imported = sample(321);
    imported.observe_limit("claude:5h", Some("0001"), 100., false, true);
    imported.observe_limit("codex:weekly", Some("0003"), 100., true, true);
    let candies = imported.inventory.candies;
    let mut result = rebase(imported, &local, BTreeMap::new());
    assert_eq!(result.lifetime_tokens, 321);
    assert_eq!(result.inventory.candies, candies);
    assert_eq!(
        result.observe_limit("claude:5h", Some("0002"), 100., false, true),
        0
    );
    assert_eq!(
        result.observe_limit("codex:weekly", Some("0003"), 100., true, true),
        0
    );
}

#[test]
fn extreme_values_sanitized_before_use() {
    let mut state = sample(u64::MAX);
    state.spent_tokens = u64::MAX;
    state.egg_tokens = u64::MAX;
    state.active.as_mut().unwrap().stage = usize::MAX;
    state.active.as_mut().unwrap().tokens = u64::MAX;
    let mut result = decode(&encode(&state, 0).unwrap()).unwrap().state;
    assert_eq!(result.lifetime_tokens, 1_000_000_000_000_000);
    assert_eq!(result.spent_tokens, 1_000_000_000_000_000);
    assert_eq!(result.egg_tokens, 1_000_000_000_000_000);
    assert_eq!(result.active.as_ref().unwrap().stage, 2);
    assert_eq!(
        result.active.as_ref().unwrap().tokens,
        1_000_000_000_000_000
    );
    result.add_growth(1000);
    assert!(result.active.is_none());
    assert_eq!(result.balance(), 0);
}

#[test]
fn snapshots_list_newest_first() {
    let temp = Temp::new();
    let snapshots = temp.snapshots();
    let older = snapshots.create(&sample(100), 1).unwrap();
    let newer = snapshots.create(&sample(200), 2).unwrap();
    let list = snapshots.list();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].path, newer.path);
    assert_eq!(list[1].path, older.path);
    assert_eq!(list[0].lifetime_tokens, 200);
    assert_eq!(read(&older.path).unwrap().state.lifetime_tokens, 100);
}

#[test]
fn snapshot_retention_keeps_newest_ten() {
    let temp = Temp::new();
    let snapshots = temp.snapshots();
    for now in 1..=15 {
        snapshots.create(&sample(now), now).unwrap();
    }
    let list = snapshots.list();
    assert_eq!(list.len(), 10);
    assert_eq!(list[0].exported_at, 15);
    assert_eq!(list[9].exported_at, 6);
}

#[test]
fn automatic_snapshots_wait_twelve_hours() {
    let temp = Temp::new();
    let snapshots = temp.snapshots();
    assert!(snapshots.maybe_create(&sample(1), 100).unwrap().is_some());
    assert!(snapshots.maybe_create(&sample(2), 3700).unwrap().is_none());
    assert!(
        snapshots
            .maybe_create(&sample(3), 100 + 13 * 3600)
            .unwrap()
            .is_some()
    );
    assert_eq!(snapshots.list().len(), 2);
}

#[test]
fn future_stamped_snapshot_does_not_block_automatic_snapshots() {
    let temp = Temp::new();
    let snapshots = temp.snapshots();
    snapshots.create(&sample(1), 1_000_000).unwrap();
    assert!(snapshots.maybe_create(&sample(2), 100).unwrap().is_some());
    assert!(snapshots.maybe_create(&sample(3), 200).unwrap().is_none());
}

#[test]
fn corrupt_snapshot_is_skipped() {
    let temp = Temp::new();
    let snapshots = temp.snapshots();
    snapshots.create(&sample(123), 1).unwrap();
    let bad = snapshots.directory.join("companion-snapshot-bad.json");
    fs::write(&bad, "{bad").unwrap();
    assert_eq!(snapshots.list().len(), 1);
    assert_eq!(read(&bad).unwrap_err(), Error::NotASave);
    assert_eq!(snapshots.list().len(), 1);
    assert_eq!(snapshots.latest().unwrap().lifetime_tokens, 123);
}

#[test]
fn same_second_snapshots_never_overwrite_original() {
    let temp = Temp::new();
    let snapshots = temp.snapshots();
    let first = snapshots.create(&sample(1), 99).unwrap();
    let second = snapshots.create(&sample(2), 99).unwrap();
    assert_ne!(first.path, second.path);
    assert_eq!(read(&first.path).unwrap().state.lifetime_tokens, 1);
    assert_eq!(read(&second.path).unwrap().state.lifetime_tokens, 2);
    assert_eq!(snapshots.list()[0].path, second.path);
}

#[test]
fn duplicate_path_keeps_current_species_in_active_and_released_records() {
    let mut state = sample(0);
    let mon = state.active.as_mut().unwrap();
    mon.path = vec![1, 1, 2];
    mon.stage = 2;
    let mut released = mon.clone();
    released.released = true;
    state.collection.push(released);
    let decoded = decode(&encode(&state, 0).unwrap()).unwrap().state;
    let active = decoded.active.unwrap();
    assert_eq!(active.path, [1, 2, 3]);
    assert_eq!(active.stage, 1);
    assert_eq!(active.species_id(), 2);
    assert_eq!(decoded.collection[0].path, [1, 2]);
    assert_eq!(decoded.collection[0].stage, 1);
    assert_eq!(decoded.collection[0].species_id(), 2);
}

#[test]
fn all_persisted_fields_have_an_explicit_transfer_policy() {
    let state = value(&sample(123));
    let progress = [
        "active",
        "egg_tokens",
        "pending_line",
        "pending_form",
        "collection",
        "lifetime_tokens",
        "spent_tokens",
        "inventory",
        "egg_tier",
        "representative",
        "representative_form",
        "representative_shiny",
        "random_seed",
    ];
    let local = ["baselines", "growth_difficulty", "shop_difficulty"];
    let merged = ["limit_baselines"];
    let classified: std::collections::BTreeSet<_> =
        progress.into_iter().chain(local).chain(merged).collect();
    assert_eq!(
        state
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
        classified,
        "A new saved field must explicitly choose progress, device-local, or merged semantics"
    );
}

#[test]
fn export_name_carries_date() {
    assert_eq!(
        suggested_file_name(1_785_758_400),
        "Shark-Pokemon-Save-2026-08-03.json"
    );
    assert_eq!(suggested_file_name(0), "Shark-Pokemon-Save-1970-01-01.json");
    assert_eq!(
        suggested_file_name(u64::MAX),
        "Shark-Pokemon-Save-9999-12-31.json"
    );
}

#[test]
fn save_errors_hide_diagnostic_detail_from_users() {
    let errors = [
        Error::NotASave,
        Error::NewerSchema(2),
        Error::TooLarge,
        Error::Io("NSError: private/path permission denied".into()),
    ];
    let messages = errors.each_ref().map(|error| error.to_string());
    assert_eq!(
        messages
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        4
    );
    assert!(messages.iter().all(|s| !s.is_empty()
        && !s.contains("NSError")
        && !s.contains("private/path")
        && !s.contains("NotASave")
        && !s.contains("couldn't be completed")));
    assert!(format!("{:?}", errors[3]).contains("permission denied"));
}
