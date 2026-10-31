//! Share GPUI bitmaps across chat activity, the footer, Home, evolution rows and the Dex.
//! Render-time lookups are memory-only; network/cache warming stays on background workers.
use crate::pokemon::sprites::Asset;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::{Bounds, DevicePixels, Img, ObjectFit, Pixels, RenderImage, point, px, size};
use gpui_kit::{
    InteractiveElement as _, IntoElement as _, ParentElement as _, StatefulInteractiveElement as _,
    Styled as _, StyledImage as _,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};
const BUDGET: usize = 64 * 1024 * 1024;
struct Entry {
    asset: Weak<Asset>,
    image: Arc<RenderImage>,
    bytes: usize,
    tick: u64,
}
#[derive(Default)]
struct Cache {
    entries: BTreeMap<(PathBuf, std::time::Duration, bool), Entry>,
    bytes: usize,
    tick: u64,
}
fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Cache::default()))
}
fn existing(path: &Path, floor: std::time::Duration, trim: bool) -> Option<Arc<RenderImage>> {
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    c.tick = c.tick.wrapping_add(1);
    let tick = c.tick;
    c.entries
        .get_mut(&(path.to_path_buf(), floor, trim))
        .map(|e| {
            e.tick = tick;
            e.image.clone()
        })
}
pub(crate) fn render_image(asset: Arc<Asset>) -> Arc<RenderImage> {
    render_variant(asset, std::time::Duration::ZERO, false)
}

pub(crate) fn render_capped(asset: Arc<Asset>, floor: std::time::Duration) -> Arc<RenderImage> {
    render_variant(asset, floor, true)
}
pub(crate) fn render_placeholder(asset: Arc<Asset>) -> Arc<RenderImage> {
    render_variant(asset, std::time::Duration::ZERO, true)
}
fn render_variant(asset: Arc<Asset>, floor: std::time::Duration, trim: bool) -> Arc<RenderImage> {
    let key = (asset.path.clone(), floor, trim);
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    c.tick = c.tick.wrapping_add(1);
    let tick = c.tick;
    if let Some(entry) = c.entries.get_mut(&key) {
        if entry
            .asset
            .upgrade()
            .is_some_and(|old| Arc::ptr_eq(&old, &asset))
        {
            entry.tick = tick;
            return entry.image.clone();
        }
    }
    if let Some(old) = c.entries.remove(&key) {
        c.bytes = c.bytes.saturating_sub(old.bytes);
    }
    let delays: Vec<_> = asset
        .frames
        .iter()
        .map(|frame| {
            let delay = std::time::Duration::from(frame.delay());
            if asset.frames.len() > 1 && delay < std::time::Duration::from_millis(20) {
                std::time::Duration::from_millis(100)
            } else {
                delay
            }
        })
        .collect();
    let frames: Vec<_> = crate::pokemon::animation::cap_frames(&delays, floor)
        .into_iter()
        .map(|(index, delay)| {
            let source = &asset.frames[index];
            let mut pixels = source.buffer().clone();
            if trim && asset.path.extension().is_some_and(|ext| ext == "png") {
                let mut x0 = pixels.width();
                let mut y0 = pixels.height();
                let mut x1 = 0;
                let mut y1 = 0;
                for (x, y, pixel) in pixels.enumerate_pixels() {
                    if pixel[3] > 0 {
                        x0 = x0.min(x);
                        y0 = y0.min(y);
                        x1 = x1.max(x);
                        y1 = y1.max(y);
                    }
                }
                if x0 <= x1 && y0 <= y1 {
                    pixels = image::imageops::crop_imm(&pixels, x0, y0, x1 - x0 + 1, y1 - y0 + 1)
                        .to_image();
                }
            }
            for pixel in pixels.pixels_mut() {
                pixel.0.swap(0, 2);
            }
            image::Frame::from_parts(
                pixels,
                source.left(),
                source.top(),
                image::Delay::from_numer_denom_ms(
                    delay.as_micros().min(u32::MAX as u128) as u32,
                    1000,
                ),
            )
        })
        .collect();
    let bytes = frames.iter().map(|f| f.buffer().len()).sum();
    let image = Arc::new(RenderImage::new(frames));
    if bytes <= BUDGET {
        while c.bytes + bytes > BUDGET || c.entries.len() >= 128 {
            let Some(key) = c
                .entries
                .iter()
                .min_by_key(|(_, e)| e.tick)
                .map(|(p, _)| p.clone())
            else {
                break;
            };
            if let Some(e) = c.entries.remove(&key) {
                c.bytes = c.bytes.saturating_sub(e.bytes);
            }
        }
        c.bytes += bytes;
        c.entries.insert(
            key,
            Entry {
                asset: Arc::downgrade(&asset),
                image: image.clone(),
                bytes,
                tick,
            },
        );
    }
    image
}
pub(crate) fn cached_frame(path: &Path, floor: std::time::Duration) -> Option<Arc<RenderImage>> {
    existing(path, floor, true).or_else(|| {
        existing(
            path,
            std::time::Duration::ZERO,
            path.extension().is_some_and(|ext| ext == "png"),
        )
    })
}
pub(crate) fn fitted_size(width: u32, height: u32, box_size: f32) -> gpui_kit::Size<Pixels> {
    let box_size = if box_size.is_finite() {
        box_size.max(0.)
    } else {
        0.
    };
    if width == 0 || height == 0 {
        return size(px(box_size), px(box_size));
    }
    ObjectFit::Contain
        .get_bounds(
            Bounds::new(point(px(0.), px(0.)), size(px(box_size), px(box_size))),
            size(DevicePixels::from(width), DevicePixels::from(height)),
        )
        .size
}
pub(crate) fn sized(path: &Path, box_size: f32) -> Img {
    let image = crate::pokemon::sprites::cached_memory(path)
        .map(|asset| {
            if path.extension().is_some_and(|ext| ext == "png") {
                render_placeholder(asset)
            } else {
                render_image(asset)
            }
        })
        .or_else(|| {
            existing(
                path,
                std::time::Duration::ZERO,
                path.extension().is_some_and(|ext| ext == "png"),
            )
        });
    if let Some(image) = image {
        let source = image.size(0);
        let fitted = fitted_size(source.width.into(), source.height.into(), box_size);
        gpui_kit::img(image)
            .w(fitted.width)
            .h(fitted.height)
            .object_fit(ObjectFit::Contain)
    } else {
        gpui_kit::img(path.to_path_buf())
            .size(px(box_size))
            .object_fit(ObjectFit::Contain)
    }
}
/// Paint a selected frame without `Img`'s display-rate animation requests. The caller owns
/// the timer, so a 400ms persistent-frame delay produces one wake every 400ms, not every vsync.
pub(crate) fn paint_frame(
    image: Arc<RenderImage>,
    index: usize,
    box_size: f32,
) -> gpui_kit::AnyElement {
    let source = image.size(index);
    let fitted = fitted_size(source.width.into(), source.height.into(), box_size);
    gpui_kit::div()
        .id("pokemon-sprite")
        .role(gpui_kit::Role::Image)
        .aria_label("Pokémon companion")
        .test_support()
        .w(fitted.width)
        .h(fitted.height)
        .child(
            gpui_kit::canvas(
                |_, _, _| (),
                move |bounds, _, window, cx| {
                    let index = if cx.reduce_motion() { 0 } else { index };
                    let fit = ObjectFit::Contain.get_bounds(bounds, image.size(index));
                    let _ = window.paint_image(
                        bounds,
                        fit,
                        gpui_kit::Corners::default(),
                        image.clone(),
                        index,
                        false,
                    );
                },
            )
            .size_full(),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    fn asset(name: &str) -> Arc<Asset> {
        Arc::new(Asset {
            path: std::env::temp_dir().join(format!("shark-image-{name}")),
            encoded: Arc::new(vec![]),
            frames: vec![
                image::Frame::new(image::RgbaImage::from_pixel(
                    36,
                    66,
                    image::Rgba([255, 64, 8, 255]),
                )),
                image::Frame::new(image::RgbaImage::from_pixel(
                    36,
                    66,
                    image::Rgba([8, 255, 64, 255]),
                )),
            ],
        })
    }
    // upstream: SpriteAspectRatioTests.swift::testNonSquareSpriteDoesNotRenderSquare
    // upstream: SpriteAspectRatioTests.swift::testFittedSizePreservesSourceAspectRatio
    // upstream: SpriteAspectRatioTests.swift::testFittedSizeFillsTheBoxOnItsLongerAxisAndNeverOverflows
    // upstream: SpriteAspectRatioTests.swift::testSquareSourceStillFillsTheSquareBox
    // upstream: SpriteAspectRatioTests.swift::testSpriteViewImageSizeMatchesFittedSize
    #[test]
    fn native_contain_preserves_every_canvas_ratio() {
        for (w, h) in [(36, 66), (50, 46), (74, 75), (96, 96), (137, 69), (57, 19)] {
            for b in [22., 40., 76., 84., 96.] {
                let f = fitted_size(w, h, b);
                let fw = f32::from(f.width);
                let fh = f32::from(f.height);
                assert!((fw / fh - w as f32 / h as f32).abs() < 0.001);
                assert!(fw <= b + 0.001 && fh <= b + 0.001);
                assert!((fw.max(fh) - b).abs() < 0.001);
                if w != h {
                    assert!((fw - fh).abs() > 0.01)
                } else {
                    assert_eq!(fw, b);
                    assert_eq!(fh, b);
                }
            }
        }
    }
    // upstream: SpriteAspectRatioTests.swift::testDegenerateSourceFallsBackToBoxWithoutDividingByZero
    #[test]
    fn empty_canvas_uses_finite_fallback_slot() {
        for (w, h) in [(0, 0), (0, 66), (36, 0)] {
            assert_eq!(fitted_size(w, h, 22.), size(px(22.), px(22.)));
        }
    }
    // upstream: SpriteAspectRatioTests.swift::testMenuBarFramesConvertToLayerBitmaps
    // upstream: SpriteAspectRatioTests.swift::testSpriteContentsScaleFollowsTheBitmapNotTheScreen
    // upstream: SpriteAnimationPreviewTests.swift::testDetailFirstRenderUsesCachedAnimationCanvasAndPixels
    // upstream: SpriteSubjectTests.swift::testMultiFrameAcceptedWhenNotCancelled
    #[test]
    fn shared_native_bitmap_preserves_frames_and_bgra_pixels() {
        let source = asset("native-bitmap");
        let first = render_image(source.clone());
        let second = render_image(source);
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.frame_count(), 2);
        assert_eq!(
            first.size(0),
            size(DevicePixels::from(36), DevicePixels::from(66))
        );
        assert_eq!(&first.as_bytes(0).unwrap()[..4], &[8, 64, 255, 255]);
        assert_eq!(&first.as_bytes(1).unwrap()[..4], &[64, 255, 8, 255]);
    }
    #[test]
    fn new_decoded_asset_at_same_path_replaces_previous_bitmap() {
        let first = render_image(asset("new-identity"));
        let second = render_image(asset("new-identity"));
        assert!(!Arc::ptr_eq(&first, &second));
    }
    // upstream: PerformanceTests.swift::testIdentityKeysIncludeTheFrameFloor
    #[test]
    fn renderer_cache_keys_include_floor_and_variant() {
        let source = asset("floor-key");
        let a = render_capped(source.clone(), std::time::Duration::from_millis(400));
        let b = render_capped(source.clone(), std::time::Duration::from_millis(100));
        assert!(!Arc::ptr_eq(&a, &b));
        assert!(Arc::ptr_eq(
            &a,
            &render_capped(source, std::time::Duration::from_millis(400))
        ));
        assert!(!Arc::ptr_eq(
            &a,
            &render_capped(
                asset("floor-key-shiny"),
                std::time::Duration::from_millis(400)
            )
        ));
    }
    // upstream: SpriteAnimationPreviewTests.swift::testAnimatedPlaceholderRemovesPaddingWithoutChangingStaticThumbnail
    // upstream: SpriteSubjectTests.swift::testSingleFrameFallsBackToStatic
    #[test]
    fn animated_placeholder_trims_padding_without_changing_static_asset() {
        let mut pixels = image::RgbaImage::new(96, 96);
        for y in 24..72 {
            for x in 24..72 {
                pixels.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
            }
        }
        let asset = Arc::new(Asset {
            path: std::env::temp_dir().join("shark-placeholder-normal.png"),
            frames: vec![image::Frame::new(pixels)],
            encoded: Arc::new(vec![]),
        });
        let full = render_image(asset.clone());
        let cropped = render_placeholder(asset.clone());
        assert_eq!(asset.dimensions(), (96, 96));
        assert_eq!(
            full.size(0),
            size(DevicePixels::from(96), DevicePixels::from(96))
        );
        assert_eq!(
            cropped.size(0),
            size(DevicePixels::from(48), DevicePixels::from(48))
        );
        assert!(Arc::ptr_eq(&cropped, &render_placeholder(asset)));
        assert_eq!(cropped.frame_count(), 1);
        assert!(!Arc::ptr_eq(&full, &cropped));
    }
}
