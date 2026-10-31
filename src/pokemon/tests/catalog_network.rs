//! Explicit opt-in counterpart to upstream's PTB_PARITY live public asset audit.
use super::super::forms;
use std::{
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

// upstream: PokemonFormTests.swift::testEveryCataloguedSpriteExistsOnPokeAPI
#[test]
#[ignore = "live public sprite decode audit; run cargo test catalogued_sprite_urls_exist -- --ignored"]
fn catalogued_sprite_urls_exist() {
    let mut urls = std::collections::BTreeSet::new();
    for &id in forms::SPECIES {
        for form in forms::forms(id) {
            for animated in [false, true] {
                for shiny in [false, true] {
                    urls.insert(forms::sprite_url(id, animated, shiny, Some(&form)));
                }
            }
        }
    }
    let urls: Vec<_> = urls.into_iter().collect();
    let next = AtomicUsize::new(0);
    let checked = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::new());
    let start = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..12 {
            scope.spawn(|| {
                loop {
                    if start.elapsed() > Duration::from_secs(180) {
                        break;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(url) = urls.get(i) else {
                        break;
                    };
                    let result = super::super::api::fetch(url, None).and_then(|bytes| {
                        super::super::sprites::decode(
                            std::path::PathBuf::from(url.rsplit('/').next().unwrap()),
                            bytes,
                        )
                        .map(|_| ())
                    });
                    if let Err(error) = result {
                        failures.lock().unwrap().push(format!("{url}: {error}"));
                    }
                    checked.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });
    assert_eq!(
        checked.load(Ordering::Relaxed),
        urls.len(),
        "public sprite audit exceeded its 3-minute total budget"
    );
    let failures = failures.into_inner().unwrap();
    assert!(
        failures.is_empty(),
        "{} of {} public sprite decode checks failed:\n{}",
        failures.len(),
        urls.len(),
        failures.join("\n")
    );
    eprintln!(
        "{} catalogued sprite URLs downloaded and decoded in {:?}",
        urls.len(),
        start.elapsed()
    );
}
