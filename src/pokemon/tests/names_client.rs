use super::*;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
#[derive(Default)]
struct Server {
    calls: AtomicUsize,
    fail: AtomicBool,
}
fn body() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"names":[{"name":"Tackle","language":{"name":"en"}},{"name":"몸통박치기","language":{"name":"ko"}},{"name":"Azione","language":{"name":"it"}}]})).unwrap()
}
fn client(path: Option<PathBuf>, server: Arc<Server>, clock: Arc<AtomicU64>) -> Client {
    Client::with_transport(
        path,
        move || clock.load(Ordering::SeqCst),
        move |_| {
            server.calls.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(20));
            if server.fail.load(Ordering::SeqCst) {
                Err("offline".into())
            } else {
                Ok(body())
            }
        },
    )
}
fn resource() -> Resource {
    Resource::new(Kind::Move, "tackle")
}
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "shark-names-{}-{}",
            std::process::id(),
            crate::pokemon::seed()
        )))
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn memory_and_disk_keep_every_language_without_refetch() {
    let dir = Temp::new();
    let server = Arc::new(Server::default());
    let clock = Arc::new(AtomicU64::new(1_800_000_000));
    let c = client(Some(dir.0.clone()), server.clone(), clock.clone());
    let first = c.names(&resource()).unwrap();
    assert_eq!(resolve_name(&first, &["ko"]).as_deref(), Some("몸통박치기"));
    assert_eq!(
        resolve_name(&c.names(&resource()).unwrap(), &["en"]).as_deref(),
        Some("Tackle")
    );
    let restored = client(Some(dir.0.clone()), server.clone(), clock);
    assert_eq!(
        resolve_name(&restored.names(&resource()).unwrap(), &["it"]).as_deref(),
        Some("Azione")
    );
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
}
#[test]
fn concurrent_rows_share_one_request() {
    let server = Arc::new(Server::default());
    let c = client(
        None,
        server.clone(),
        Arc::new(AtomicU64::new(1_800_000_000)),
    );
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let values = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            c.names(&resource()).unwrap()
        });
        let b = scope.spawn(|| {
            barrier.wait();
            c.names(&resource()).unwrap()
        });
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_eq!(values.0, values.1);
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
}
#[test]
fn stale_offline_fallback_keeps_original_timestamp_and_retries() {
    let dir = Temp::new();
    let server = Arc::new(Server::default());
    let clock = Arc::new(AtomicU64::new(1_800_000_000));
    let c = client(Some(dir.0.clone()), server.clone(), clock.clone());
    c.names(&resource()).unwrap();
    clock.fetch_add(31 * 86400, Ordering::SeqCst);
    server.fail.store(true, Ordering::SeqCst);
    let stale = client(Some(dir.0.clone()), server.clone(), clock);
    assert_eq!(stale.names(&resource()).unwrap()["ko"], "몸통박치기");
    let snapshot: Snapshot =
        serde_json::from_slice(&fs::read(dir.0.join("move-tackle.json")).unwrap()).unwrap();
    assert_eq!(snapshot.fetched_at, 1_800_000_000);
    server.fail.store(false, Ordering::SeqCst);
    stale.names(&resource()).unwrap();
    assert_eq!(server.calls.load(Ordering::SeqCst), 3);
}
#[test]
fn cold_failure_retries_and_invalid_resources_never_fetch() {
    let server = Arc::new(Server::default());
    let c = client(
        None,
        server.clone(),
        Arc::new(AtomicU64::new(1_800_000_000)),
    );
    server.fail.store(true, Ordering::SeqCst);
    assert!(c.names(&resource()).is_err());
    server.fail.store(false, Ordering::SeqCst);
    assert_eq!(c.names(&resource()).unwrap()["en"], "Tackle");
    for name in [
        "",
        "../move/tackle",
        "tackle?language=ko",
        "tackle/../../",
        "TACKLE",
    ] {
        assert!(c.names(&Resource::new(Kind::Move, name)).is_err());
    }
    assert!(serde_json::from_str::<Kind>("\"../move\"").is_err());
    assert_eq!(server.calls.load(Ordering::SeqCst), 2);
}
#[test]
fn kinds_have_distinct_cache_keys_and_corruption_recovers() {
    let dir = Temp::new();
    fs::create_dir_all(&dir.0).unwrap();
    fs::write(dir.0.join("move-tackle.json"), "broken").unwrap();
    let server = Arc::new(Server::default());
    let c = client(
        Some(dir.0.clone()),
        server.clone(),
        Arc::new(AtomicU64::new(1_800_000_000)),
    );
    for kind in [Kind::Move, Kind::Ability, Kind::Type] {
        c.names(&Resource::new(kind, "tackle")).unwrap();
    }
    assert_eq!(server.calls.load(Ordering::SeqCst), 3);
}
#[test]
fn form_resources_use_bare_form_names_and_exact_endpoints() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let urls = captured.clone();
    let c = Client::with_transport(
        None,
        || 1_800_000_000,
        move |url| {
            urls.lock().unwrap().push(url.to_owned());
            Ok(serde_json::to_vec(&serde_json::json!({"names":[{"name":"Shaymin Céleste","language":{"name":"fr"}}],"form_names":[{"name":"Forme Céleste","language":{"name":"fr"}},{"name":"Sky Forme","language":{"name":"en"}}]})).unwrap())
        },
    );
    let sky = crate::pokemon::forms::Form("sky".into());
    let id = crate::pokemon::forms::name_resource(492, Some(&sky)).unwrap();
    let names = c
        .names(&Resource::new(Kind::PokemonForm, id.to_string()))
        .unwrap();
    assert_eq!(
        names,
        BTreeMap::from([
            ("fr".into(), "Forme Céleste".into()),
            ("en".into(), "Sky Forme".into())
        ])
    );
    assert_eq!(
        *captured.lock().unwrap(),
        ["https://pokeapi.co/api/v2/pokemon-form/10064/"]
    );
    assert_eq!(
        crate::pokemon::forms::name_resource(201, Some(&crate::pokemon::forms::Form("b".into()))),
        None
    );
    assert_eq!(crate::pokemon::forms::name_resource(25, None), None);
    for (species, expected) in [
        (492, vec![492, 10064]),
        (201, vec![]),
        (479, vec![10058, 10059, 10060, 10061, 10062]),
    ] {
        let ids = crate::pokemon::forms::forms(species)
            .iter()
            .filter_map(|form| crate::pokemon::forms::name_resource(species, Some(form)))
            .collect::<Vec<_>>();
        assert_eq!(ids, expected);
    }
}
#[test]
fn pending_labels_do_not_flash_english_and_reopen_from_shared_snapshot() {
    let items = [Item {
        resource: resource(),
        suffix: String::new(),
    }];
    let mut snapshots = BTreeMap::new();
    let failed = BTreeSet::new();
    assert_eq!(text(&items, &["ko"], &snapshots, &failed), "…");
    snapshots.insert(
        resource(),
        BTreeMap::from([
            ("ko".into(), "몸통박치기".into()),
            ("en".into(), "Tackle".into()),
        ]),
    );
    assert_eq!(text(&items, &["ko"], &snapshots, &failed), "몸통박치기");
    for language in ["en", "pt"] {
        assert_eq!(text(&items, &[language], &snapshots, &failed), "Tackle");
    }
    let joined = [
        items[0].clone(),
        Item {
            resource: Resource::new(Kind::Type, "grass"),
            suffix: String::new(),
        },
    ];
    assert_eq!(
        text(&joined, &["ko"], &snapshots, &failed),
        "몸통박치기 · …"
    );
}
#[test]
fn failed_labels_use_identifiers_without_poisoning_snapshots() {
    let items = [Item {
        resource: resource(),
        suffix: String::new(),
    }];
    let failed = BTreeSet::from([resource()]);
    let mut snapshots = BTreeMap::new();
    assert_eq!(text(&items, &["ko"], &snapshots, &failed), "Tackle");
    assert!(snapshots.is_empty());
    snapshots.insert(
        resource(),
        BTreeMap::from([("ko".into(), "몸통박치기".into())]),
    );
    assert_eq!(text(&items, &["ko"], &snapshots, &failed), "몸통박치기");
}
#[test]
fn every_language_resolves_joined_metadata_without_changing_keys() {
    let ability = Resource::new(Kind::Ability, "overgrow");
    let grass = Resource::new(Kind::Type, "grass");
    let maps = BTreeMap::from([
        (
            resource(),
            BTreeMap::from([
                ("en".into(), "Tackle".into()),
                ("ko".into(), "몸통박치기".into()),
                ("ja-hrkt".into(), "たいあたり".into()),
                ("es".into(), "Placaje".into()),
                ("fr".into(), "Charge".into()),
                ("de".into(), "Tackle".into()),
            ]),
        ),
        (
            ability.clone(),
            BTreeMap::from([
                ("en".into(), "Overgrow".into()),
                ("ko".into(), "심록".into()),
            ]),
        ),
        (
            grass.clone(),
            BTreeMap::from([("en".into(), "Grass".into()), ("ko".into(), "풀".into())]),
        ),
    ]);
    let items = [
        Item {
            resource: ability.clone(),
            suffix: String::new(),
        },
        Item {
            resource: resource(),
            suffix: String::new(),
        },
        Item {
            resource: grass,
            suffix: String::new(),
        },
    ];
    assert_eq!(
        text(&items, &["ko"], &maps, &BTreeSet::new()),
        "심록 · 몸통박치기 · 풀"
    );
    assert_eq!(
        text(&items, &["pt-br", "pt"], &maps, &BTreeSet::new()),
        "Overgrow · Tackle · Grass"
    );
    assert_eq!(
        text(&items, &["ko"], &BTreeMap::new(), &BTreeSet::new()),
        "… · … · …"
    );
    let hidden = [Item {
        resource: ability,
        suffix: " (숨겨진 특성)".into(),
    }];
    assert_eq!(
        text(&hidden, &["ko"], &maps, &BTreeSet::new()),
        "심록 (숨겨진 특성)"
    );
    assert_eq!(items[0].resource.name, "overgrow");
    for language in ["ko", "en", "ja-hrkt", "es", "fr", "pt-br", "de", "ru"] {
        assert!(!text(&items, &[language], &maps, &BTreeSet::new()).is_empty());
    }
}
#[test]
fn distinct_metadata_requests_are_bounded() {
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let a = active.clone();
    let m = maximum.clone();
    let c = Client::with_transport(
        None,
        || 1_800_000_000,
        move |_| {
            let n = a.fetch_add(1, Ordering::SeqCst) + 1;
            m.fetch_max(n, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(10));
            a.fetch_sub(1, Ordering::SeqCst);
            Ok(body())
        },
    );
    std::thread::scope(|scope| {
        for i in 0..24 {
            let c = c.clone();
            scope.spawn(move || {
                c.names(&Resource::new(Kind::Move, format!("move-{i}")))
                    .unwrap();
            });
        }
    });
    assert!(maximum.load(Ordering::SeqCst) <= MAX_REQUESTS);
}

#[test]
fn concurrent_failure_is_broadcast_and_does_not_poison_retry() {
    let server = Arc::new(Server::default());
    server.fail.store(true, Ordering::SeqCst);
    let c = client(
        None,
        server.clone(),
        Arc::new(AtomicU64::new(1_800_000_000)),
    );
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            c.names(&resource())
        });
        let b = scope.spawn(|| {
            barrier.wait();
            c.names(&resource())
        });
        assert!(a.join().unwrap().is_err());
        assert!(b.join().unwrap().is_err());
    });
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
    server.fail.store(false, Ordering::SeqCst);
    assert_eq!(c.names(&resource()).unwrap()["en"], "Tackle");
    assert_eq!(server.calls.load(Ordering::SeqCst), 2);
}
#[test]
fn preferred_language_codes_normalize_whitespace_and_legacy_aliases() {
    let names = BTreeMap::from([
        ("en".into(), "Tackle".into()),
        ("ko".into(), "몸통박치기".into()),
        ("pt_BR".into(), "Investida".into()),
    ]);
    assert_eq!(
        resolve_name(&names, &[" KO \n"]).as_deref(),
        Some("몸통박치기")
    );
    assert_eq!(
        resolve_name(&names, &[" PT-br "]).as_deref(),
        Some("Investida")
    );
}
