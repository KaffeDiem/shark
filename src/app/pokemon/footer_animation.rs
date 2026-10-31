//! Persistent footer frames use a deadline timer, not native Img's per-vsync animation loop.
use super::*;
use std::sync::Arc;
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Key {
    pub path: PathBuf,
    floor: Duration,
    bitmap: usize,
    animate: bool,
}
impl Shark {
    pub(crate) fn pokemon_window_activity(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.pokemon_ui.window_active = window.is_window_active();
        self.refresh_pokemon_animation(cx);
    }
    pub(crate) fn refresh_pokemon_animation(&mut self, cx: &mut Context<Self>) {
        let source = self
            .pokemon_ui
            .sprite
            .as_ref()
            .filter(|_| !self.state.pokemon_disabled)
            .and_then(|path| {
                crate::pokemon::sprites::cached_memory(path).or_else(|| {
                    self.pokemon_ui
                        .footer_asset
                        .as_ref()
                        .filter(|asset| &asset.path == path)
                        .cloned()
                })
            });
        let Some(source) = source else {
            self.pokemon_ui.footer_task.take();
            self.pokemon_ui.footer_key = None;
            self.pokemon_ui.footer_render = None;
            self.pokemon_ui.footer_asset = None;
            self.pokemon_ui.footer_frame = 0;
            return;
        };
        let floor = self
            .state
            .pokemon_animation_quality
            .effective_floor(self.pokemon_ui.low_power);
        let can_animate = self.pokemon_ui.window_active && !cx.reduce_motion();
        if self
            .pokemon_ui
            .footer_asset
            .as_ref()
            .is_some_and(|old| Arc::ptr_eq(old, &source))
            && self.pokemon_ui.footer_key.as_ref().is_some_and(|key| {
                key.path == source.path
                    && key.floor == floor
                    && key.animate
                        == (can_animate
                            && self
                                .pokemon_ui
                                .footer_render
                                .as_ref()
                                .is_some_and(|image| image.frame_count() > 1))
            })
        {
            return;
        }
        let path = source.path.clone();
        let image = images::render_capped(source.clone(), floor);
        self.pokemon_ui.footer_asset = Some(source);
        let animate = !self.state.pokemon_disabled
            && self.pokemon_ui.window_active
            && !cx.reduce_motion()
            && image.frame_count() > 1;
        let key = Key {
            path,
            floor,
            bitmap: Arc::as_ptr(&image) as usize,
            animate,
        };
        if self.pokemon_ui.footer_key.as_ref() == Some(&key) {
            return;
        }
        self.pokemon_ui.footer_task.take();
        self.pokemon_ui.footer_key = Some(key.clone());
        self.pokemon_ui.footer_render = Some(image.clone());
        self.pokemon_ui.footer_frame = 0;
        if !animate {
            return;
        }
        self.pokemon_ui.footer_task = Some(cx.spawn(async move |this, cx| {
            let delays: Vec<_> = (0..image.frame_count())
                .map(|i| Duration::from(image.delay(i)))
                .collect();
            let epoch = cx.background_executor().now();
            loop {
                let elapsed = cx
                    .background_executor()
                    .now()
                    .saturating_duration_since(epoch);
                let (_, wait) = crate::pokemon::animation::frame_at(&delays, elapsed);
                cx.background_executor().timer(wait).await;
                let elapsed = cx
                    .background_executor()
                    .now()
                    .saturating_duration_since(epoch);
                let (index, _) = crate::pokemon::animation::frame_at(&delays, elapsed);
                let keep = this
                    .update(cx, |s, cx| {
                        if s.pokemon_ui.footer_key.as_ref() != Some(&key)
                            || s.state.pokemon_disabled
                        {
                            return false;
                        }
                        if !s.pokemon_ui.window_active || cx.reduce_motion() {
                            s.refresh_pokemon_animation(cx);
                            cx.notify();
                            return false;
                        }
                        if s.pokemon_ui.footer_frame != index {
                            s.pokemon_ui.footer_frame = index;
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        }));
    }
}

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use super::*;
    use crate::app::ui_tests::{UI_LOCK, boot, test_home};
    use crate::pokemon::{Balance, Companion, EvoLine, EvoNode, Rarity, animation::Quality};
    use core::prelude::v1::test;
    use gpui_kit::test::TestWindowExt;
    fn asset(directory: &Path) -> Arc<crate::pokemon::sprites::Asset> {
        let mut bytes = vec![];
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            let frames = (0..4).map(|i| {
                image::Frame::from_parts(
                    image::RgbaImage::from_pixel(
                        4,
                        8,
                        if i % 2 == 0 {
                            image::Rgba([255, 0, 0, 255])
                        } else {
                            image::Rgba([0, 255, 0, 255])
                        },
                    ),
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(100, 1),
                )
            });
            encoder.encode_frames(frames).unwrap();
        }
        crate::pokemon::sprites::load_with(
            directory,
            crate::pokemon::sprites::Request::pokemon(25, true, false, None),
            |_| Ok(bytes.clone()),
        )
        .unwrap()
    }
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn persistent_frames_follow_quality_and_stop_when_inactive_reduced_or_disabled(
        cx: &mut TestAppContext,
    ) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = test_home();
        let image = asset(&home.join("footer-energy-fixture"));
        let mut state = State::default();
        state.pokemon_animation_quality = Quality::Smooth;
        let mut p = Companion::default();
        p.egg_tokens = Balance::EGG_HATCH_THRESHOLD;
        p.hatch(EvoLine {
            base_id: 25,
            tree: EvoNode {
                species_id: 25,
                children: vec![],
            },
            rarity: Rarity::Common,
            names: BTreeMap::new(),
        });
        p.active.as_mut().unwrap().shiny = false;
        state.pokemon = p;
        let (window, shark) = boot(cx, state, WindowOptions::default());
        cx.update_window(window, |_, _, cx| {
            shark.update(cx, |s, cx| {
                s.cancel_pokemon_sprite();
                s.pokemon_ui.sprite = Some(image.path.clone());
                s.pokemon_ui.sprite_key = Some((25, false, None));
                s.pokemon_ui.window_active = true;
                s.pokemon_ui.low_power = false;
                cx.set_reduce_motion(false);
                s.refresh_pokemon_animation(cx);
                assert!(s.pokemon_ui.footer_task.is_some());
                assert_eq!(
                    s.pokemon_ui.footer_render.as_ref().unwrap().frame_count(),
                    4
                );
            })
        })
        .unwrap();
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        cx.update_window(window, |_, _, cx| {
            shark.update(cx, |s, cx| {
                assert_eq!(s.pokemon_ui.footer_frame, 1);
                s.state.pokemon_animation_quality = Quality::PowerSaver;
                s.refresh_pokemon_animation(cx);
                assert_eq!(
                    s.pokemon_ui.footer_render.as_ref().unwrap().frame_count(),
                    1
                );
                assert!(s.pokemon_ui.footer_task.is_none());
                s.state.pokemon_animation_quality = Quality::Smooth;
                s.refresh_pokemon_animation(cx);
                assert!(s.pokemon_ui.footer_task.is_some());
                s.pokemon_ui.window_active = false;
                s.refresh_pokemon_animation(cx);
                assert!(s.pokemon_ui.footer_task.is_none());
                s.pokemon_ui.window_active = true;
                cx.set_reduce_motion(true);
                s.refresh_pokemon_animation(cx);
                assert!(s.pokemon_ui.footer_task.is_none());
                assert_eq!(s.pokemon_ui.footer_frame, 0);
                cx.set_reduce_motion(false);
                s.refresh_pokemon_animation(cx);
                assert!(s.pokemon_ui.footer_task.is_some());
                s.set_pokemon_enabled(false, cx);
                assert!(s.pokemon_ui.footer_task.is_none());
                assert!(s.pokemon_ui.power_task.is_none());
            })
        })
        .unwrap();
        cx.executor().advance_clock(Duration::from_secs(60));
        cx.run_until_parked();
        cx.update_window(window, |_, _, cx| {
            assert!(shark.read(cx).pokemon_ui.footer_task.is_none());
            assert!(shark.read(cx).pokemon_ui.power_task.is_none());
        })
        .unwrap();
    }
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn render_resumes_motion_and_rebuilds_quality_after_cache_eviction(
        cx: &mut TestAppContext,
    ) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = test_home();
        let image = asset(&home.join("retained-footer-fixture"));
        let mut state = State::default();
        state.pokemon_animation_quality = Quality::Smooth;
        state.pokemon.egg_tokens = Balance::EGG_HATCH_THRESHOLD;
        state.pokemon.hatch(EvoLine {
            base_id: 25,
            tree: EvoNode {
                species_id: 25,
                children: vec![],
            },
            rarity: Rarity::Common,
            names: BTreeMap::new(),
        });
        state.pokemon.active.as_mut().unwrap().shiny = false;
        let (window, shark) = boot(cx, state, WindowOptions::default());
        cx.update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| {
                s.cancel_pokemon_sprite();
                s.pokemon_ui.sprite = Some(image.path.clone());
                s.pokemon_ui.sprite_key = Some((25, false, None));
                s.pokemon_ui
                    .thumbnails
                    .insert((25, false, None), image.path.clone());
                s.pokemon_ui.window_active = true;
                s.pokemon_ui.low_power = false;
                cx.set_reduce_motion(false);
            });
            w.render_frame(cx);
            assert!(shark.read(cx).pokemon_ui.footer_task.is_some());
            assert!(Arc::ptr_eq(
                shark.read(cx).pokemon_ui.footer_asset.as_ref().unwrap(),
                &image
            ));
        })
        .unwrap();
        // Entry-count pressure evicts the global Asset cache, while this mounted footer owns one Arc.
        for species in 1..=300 {
            crate::pokemon::sprites::load_with(
                &home.join("sprite-lru-pressure"),
                crate::pokemon::sprites::Request::pokemon(species, true, false, None),
                |_| Ok(image.encoded.as_ref().clone()),
            )
            .unwrap();
        }
        assert!(crate::pokemon::sprites::cached_memory(&image.path).is_none());
        cx.update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| {
                s.state.pokemon_animation_quality = Quality::Balanced;
                cx.notify();
            });
            w.render_frame(cx);
            assert_eq!(
                shark
                    .read(cx)
                    .pokemon_ui
                    .footer_render
                    .as_ref()
                    .unwrap()
                    .frame_count(),
                2
            );
            assert!(shark.read(cx).pokemon_ui.footer_task.is_some());
            cx.set_reduce_motion(true);
            w.render_frame(cx);
            assert!(shark.read(cx).pokemon_ui.footer_task.is_none());
            assert_eq!(shark.read(cx).pokemon_ui.footer_frame, 0);
            cx.set_reduce_motion(false);
            w.render_frame(cx);
            assert!(shark.read(cx).pokemon_ui.footer_task.is_some());
            shark.update(cx, |s, cx| {
                s.pokemon_ui.low_power = true;
                cx.notify();
            });
            w.render_frame(cx);
            assert_eq!(
                shark
                    .read(cx)
                    .pokemon_ui
                    .footer_render
                    .as_ref()
                    .unwrap()
                    .frame_count(),
                1
            );
            assert!(shark.read(cx).pokemon_ui.footer_task.is_none());
            // Returning from another subject must queue disk decode instead of using native uncapped GIF.
            shark.update(cx, |s, cx| {
                s.state.pokemon.active.as_mut().unwrap().shiny = true;
                s.load_pokemon_sprite(cx);
                s.state.pokemon.active.as_mut().unwrap().shiny = false;
                s.load_pokemon_sprite(cx);
                assert_eq!(s.pokemon_ui.sprite.as_ref(), Some(&image.path));
                assert!(s.pokemon_ui.sprite_pending);
                assert!(s.pokemon_ui.footer_render.is_none());
                assert!(s.pokemon_ui.footer_asset.is_none());
                s.set_pokemon_enabled(false, cx);
                assert!(s.pokemon_ui.footer_asset.is_none());
            });
        })
        .unwrap();
    }
}
