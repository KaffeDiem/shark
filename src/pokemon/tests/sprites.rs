use super::*;
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "shark-sprites-{}-{}",
            std::process::id(),
            crate::pokemon::seed()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn write(&self, r: &Request, w: u32, h: u32) -> PathBuf {
        let path = self.0.join(r.filename());
        fs::write(&path, pixels(w, h, r.animated)).unwrap();
        path
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn pixels(width: u32, height: u32, animated: bool) -> Vec<u8> {
    let mut out = Cursor::new(vec![]);
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba([120, 180, 240, 255]));
    if animated {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut out);
        for delay in [100, 200] {
            encoder
                .encode_frame(Frame::from_parts(
                    image.clone(),
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(delay, 1),
                ))
                .unwrap();
        }
    } else {
        image.write_to(&mut out, ImageFormat::Png).unwrap()
    }
    out.into_inner()
}
fn form(name: &str) -> Form {
    Form(name.into())
}
fn off(_: &str) -> Result<Vec<u8>, String> {
    Err("offline".into())
}
// upstream: UnownSpriteTests.swift::testExistingUnownAndExplicitAKeepLegacyCacheKeys
// upstream: UnownSpriteTests.swift::testAll28FormsHaveIndependentNormalShinyAndAnimatedCaches
// upstream: UnownSpriteTests.swift::testFormURLsMatchPokeAPIAssetsInAllFourDirectories
// upstream: UnownSpriteTests.swift::testOtherSpeciesIgnoreAnUnownForm
#[test]
fn unown_all_variants_have_distinct_safe_paths_and_official_urls() {
    let mut keys = std::collections::BTreeSet::new();
    for f in forms::forms(201) {
        for animated in [false, true] {
            for shiny in [false, true] {
                let r = Request::pokemon(201, animated, shiny, Some(&f));
                assert!(keys.insert(r.filename()));
                let asset = if f.0 == "a" {
                    "201".into()
                } else {
                    format!("201-{}", f.0)
                };
                assert!(
                    r.url()
                        .ends_with(&format!("{asset}.{}", if animated { "gif" } else { "png" }))
                );
                assert_eq!(r.url().contains("/shiny/"), shiny);
                assert_eq!(r.url().contains("/animated/"), animated);
            }
        }
    }
    assert_eq!(keys.len(), 112);
    for animated in [false, true] {
        for shiny in [false, true] {
            assert_eq!(
                Request::pokemon(201, animated, shiny, None).filename(),
                Request::pokemon(201, animated, shiny, Some(&form("a"))).filename()
            );
            let normal = Request::pokemon(25, animated, shiny, None);
            let ignored = Request::pokemon(25, animated, shiny, Some(&form("question")));
            assert_eq!(normal.filename(), ignored.filename());
            assert_eq!(normal.url(), ignored.url());
        }
    }
}
// upstream: SpriteImageCacheTests.swift::testSynchronousLoadsReuseImagesAndKeepVariantsSeparate
// upstream: UnownSpriteTests.swift::testByteAndImageCachesKeepLettersSeparateAcrossAllFormats
#[test]
fn decoded_cache_reuses_frames_and_separates_forms_colors_formats_and_directories() {
    let t = Temp::new();
    let other = Temp::new();
    let mut all = vec![];
    for (index, name) in ["a", "b", "question"].into_iter().enumerate() {
        for animated in [false, true] {
            for shiny in [false, true] {
                let r = Request::pokemon(201, animated, shiny, Some(&form(name)));
                let path = t.write(&r, 4 + index as u32, 6 + index as u32);
                let a = cached(&path).unwrap();
                assert_eq!(a.dimensions(), (4 + index as u32, 6 + index as u32));
                assert!(all.iter().all(|old| !Arc::ptr_eq(old, &a)));
                fs::remove_file(&path).unwrap();
                assert!(Arc::ptr_eq(&a, &cached(&path).unwrap()));
                all.push(a);
            }
        }
    }
    let r = Request::pokemon(201, false, false, None);
    let path = other.write(&r, 9, 9);
    assert!(!Arc::ptr_eq(&all[0], &cached(&path).unwrap()));
}
// upstream: SpriteImageCacheTests.swift::testMissingOrInvalidImageDoesNotPreventALaterLoad
#[test]
fn misses_and_bad_images_do_not_poison_later_reads() {
    let t = Temp::new();
    let r = Request::pokemon(25, false, false, None);
    let path = t.0.join(r.filename());
    assert!(cached(&path).is_none());
    fs::write(&path, b"invalid").unwrap();
    assert!(cached(&path).is_none());
    t.write(&r, 2, 2);
    assert_eq!(cached(&path).unwrap().dimensions(), (2, 2));
}
// upstream: SpriteImageCacheTests.swift::testNormalFallbackDoesNotMaskALaterShinyImage
// upstream: UnownSpriteTests.swift::testShinyStaticFallbackKeepsLetterAndDoesNotHideALaterShiny
#[test]
fn fallback_keeps_the_same_form_and_never_masks_a_later_shiny() {
    let t = Temp::new();
    t.write(&Request::pokemon(201, false, false, None), 12, 12);
    let shiny = Request::pokemon(201, false, true, Some(&form("b")));
    assert!(cached_request(&t.0, shiny.clone()).is_none());
    let normal = Request::pokemon(201, false, false, Some(&form("b")));
    let normalpath = t.write(&normal, 4, 6);
    let first = cached_request(&t.0, shiny.clone()).unwrap();
    assert_eq!(first.path, normalpath);
    let shinypath = t.write(&shiny, 5, 7);
    let second = cached_request(&t.0, shiny).unwrap();
    assert_eq!(second.path, shinypath);
    assert!(!Arc::ptr_eq(&first, &second));
    assert!(Arc::ptr_eq(&first, &cached(&normalpath).unwrap()));
}
// upstream: SpriteImageCacheTests.swift::testAsyncLoadsPopulateTheSynchronousCacheAndReuseEachOther
// upstream: SpriteImageCacheTests.swift::testItemLoadsShareTheImageCacheInBothDirections
#[test]
fn worker_loads_and_synchronous_lookups_share_decoded_identity() {
    let t = Temp::new();
    for r in [
        Request::pokemon(25, true, false, None),
        Request::pokemon(25, false, true, None),
        Request::item("rare-candy"),
        Request::item("shiny-charm"),
    ] {
        let path = t.write(&r, 4, 6);
        let assets = std::thread::scope(|scope| {
            let a = scope.spawn(|| load_with(&t.0, r.clone(), off).unwrap());
            let b = scope.spawn(|| load_with(&t.0, r.clone(), off).unwrap());
            (a.join().unwrap(), b.join().unwrap())
        });
        assert!(Arc::ptr_eq(&assets.0, &assets.1));
        fs::remove_file(&path).unwrap();
        assert!(Arc::ptr_eq(&assets.0, &cached(&path).unwrap()));
        assert!(Arc::ptr_eq(&assets.0, &load_with(&t.0, r, off).unwrap()));
    }
}
// upstream: SpriteImageCacheTests.swift::testAsyncFallbacksStillReachNormalStaticImages
// upstream: UnownSpriteTests.swift::testAsyncAnimationAndShinyImageFallbacksKeepSelectedLetter
// upstream: UnownSpriteTests.swift::testDecodedShinyFallbackUsesTheSameLetter
#[test]
fn worker_fallback_reaches_same_form_normal_static_with_no_key_pollution() {
    let t = Temp::new();
    let r = Request::pokemon(201, true, true, Some(&form("b")));
    for candidate in r.candidates() {
        fs::write(t.0.join(candidate.filename()), b"invalid").unwrap();
    }
    t.write(&Request::pokemon(201, false, false, None), 12, 12);
    let normal = Request::pokemon(201, false, false, Some(&form("b")));
    let normalpath = t.write(&normal, 4, 7);
    let asset = load_with(&t.0, r.clone(), off).unwrap();
    assert_eq!(asset.path, normalpath);
    assert_eq!(asset.dimensions(), (4, 7));
    assert!(!is_exact_animated(&asset.path, 201, true, Some(&form("b"))));
    assert!(cached(&t.0.join(r.filename())).is_none());
    let exact = t.write(&r, 5, 8);
    let refreshed = load_with(&t.0, r, off).unwrap();
    assert_eq!(refreshed.path, exact);
    assert!(is_exact_animated(
        &refreshed.path,
        201,
        true,
        Some(&form("b"))
    ));
}
// upstream: UnownSpriteTests.swift::testDecodedAnimationCacheKeepsLettersColorsAndCanvasSeparate
#[test]
fn decoded_animations_keep_canvas_delays_and_variant_identity() {
    let t = Temp::new();
    let mut all = vec![];
    for (index, name) in ["a", "b", "question"].into_iter().enumerate() {
        for shiny in [false, true] {
            let r = Request::pokemon(201, true, shiny, Some(&form(name)));
            let a = load_with(&t.0, r, |_| {
                Ok(pixels(4 + index as u32, 7 + index as u32, true))
            })
            .unwrap();
            assert_eq!(a.frames.len(), 2);
            assert_eq!(a.dimensions(), (4 + index as u32, 7 + index as u32));
            assert_eq!(a.frames[0].delay().numer_denom_ms(), (100, 1));
            assert_eq!(a.frames[1].delay().numer_denom_ms(), (200, 1));
            assert!(all.iter().all(|old| !Arc::ptr_eq(old, &a)));
            all.push(a);
        }
    }
}
#[test]
fn unsafe_items_oversized_and_mismatched_images_are_rejected() {
    let t = Temp::new();
    for name in ["../secret", "foo/bar", "", "UPPER"] {
        assert!(
            load_with(&t.0, Request::item(name), |_| panic!(
                "unsafe name cannot reach fetch"
            ))
            .is_err()
        );
    }
    assert!(decode(t.0.join("x.png"), vec![0; MAX_FILE + 1]).is_err());
    assert!(decode(t.0.join("x.gif"), pixels(2, 2, false)).is_err());
    assert!(decode(t.0.join("x.png"), pixels(1025, 1, false)).is_err());
}

// upstream: SpriteAnimationRecoveryTests.swift::testCancelledShinyRequestDoesNotStartNormalFallbackOrApplyAResult
#[test]
fn cancellation_prevents_new_fallbacks_and_cache_publication() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let t = Temp::new();
    let request = Request::pokemon(25, true, true, None);
    let cancel = AtomicBool::new(true);
    assert!(
        load_cancellable_with(&t.0, request.clone(), &cancel, |_| panic!(
            "cancelled request must not fetch"
        ))
        .is_err()
    );
    cancel.store(false, Ordering::Relaxed);
    let mut calls = 0;
    assert!(
        load_cancellable_with(&t.0, request.clone(), &cancel, |_| {
            calls += 1;
            cancel.store(true, Ordering::Relaxed);
            Ok(pixels(2, 2, true))
        })
        .is_err()
    );
    assert_eq!(calls, 1);
    assert!(!t.0.join(request.filename()).exists());
}
#[test]
fn gif_delays_under_twenty_milliseconds_normalize_to_one_hundred() {
    let t = Temp::new();
    let mut bytes = Cursor::new(vec![]);
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
        for delay in [0, 10, 20, 100] {
            encoder
                .encode_frame(Frame::from_parts(
                    image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 0, 255])),
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(delay, 1),
                ))
                .unwrap();
        }
    }
    let decoded = decode(t.0.join("delays.gif"), bytes.into_inner()).unwrap();
    assert_eq!(
        decoded
            .frames
            .iter()
            .map(|f| f.delay().numer_denom_ms())
            .collect::<Vec<_>>(),
        [(100, 1), (100, 1), (20, 1), (100, 1)]
    );
}

// upstream: SpriteAnimationRecoveryTests.swift::testShinyMissStillUsesNormalGIFWithoutWaiting
#[test]
fn shiny_gif_miss_uses_normal_animation_in_the_same_attempt() {
    let t = Temp::new();
    let mut paths = Vec::new();
    let asset = load_with(&t.0, Request::pokemon(176, true, true, None), |url| {
        paths.push(url.to_string());
        if url.contains("/shiny/") {
            Err("missing shiny GIF".into())
        } else {
            Ok(pixels(4, 6, true))
        }
    })
    .unwrap();
    assert_eq!(paths.len(), 2);
    assert!(paths[0].contains("/animated/shiny/"));
    assert!(paths[1].contains("/animated/"));
    assert!(!paths[1].contains("/shiny/"));
    assert_eq!(asset.frames.len(), 2);
    assert_eq!(asset.path.file_name().unwrap(), "sprite-176-normal.gif");
}

// upstream: SpriteAnimationRecoveryTests.swift::testWarmShinyFormCacheSkipsFallbackNetworkAndBackoff
#[test]
fn warm_shiny_form_animation_is_immediately_available_without_network() {
    let t = Temp::new();
    let request = Request::pokemon(201, true, true, Some(&form("b")));
    let path = t.write(&request, 4, 6);
    let asset = load_with(&t.0, request, |_| {
        panic!("warm preferred sprite must not fetch")
    })
    .unwrap();
    assert_eq!(asset.frames.len(), 2);
    assert_eq!(asset.path, path);
}

// upstream: SpriteAnimationRecoveryTests.swift::testUnsupportedSpeciesLoadsStaticOnceWithoutRetry
#[test]
fn species_outside_the_gen_one_to_five_catalog_never_start_network() {
    let t = Temp::new();
    for species in [0, 650, 1000, u16::MAX] {
        for animated in [false, true] {
            assert!(
                load_with(
                    &t.0,
                    Request::pokemon(species, animated, false, None),
                    |_| panic!("unsupported species must not fetch")
                )
                .is_err()
            );
        }
    }
}

#[test]
fn cancellation_during_disk_decode_does_not_publish_decoded_frames() {
    let t = Temp::new();
    let path = t.write(&Request::pokemon(25, true, false, None), 4, 8);
    let calls = std::cell::Cell::new(0);
    assert!(
        cached_checked(&path, || {
            let n = calls.get();
            calls.set(n + 1);
            n == 0
        })
        .is_none()
    );
    assert_eq!(
        calls.get(),
        2,
        "cancellation is checked again after decoding"
    );
    assert!(cached_memory(&path).is_none());
    assert_eq!(cached(&path).unwrap().frames.len(), 2);
}
