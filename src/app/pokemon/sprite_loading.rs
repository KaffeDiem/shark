//! Sprite publication follows the requested appearance; late work may warm disk but cannot replace it.
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
pub(super) fn retry_delay(attempt: u8) -> Duration {
    Duration::from_secs((2u64.saturating_pow(u32::from(attempt.min(5)) + 1)).min(30))
}
impl Shark {
    pub(super) fn cancel_pokemon_sprite(&mut self) {
        if let Some(cancel) = self.pokemon_ui.sprite_cancel.take() {
            cancel.store(true, Ordering::SeqCst);
        }
        self.pokemon_ui.sprite_task.take();
        self.pokemon_ui.sprite_pending = false;
        self.pokemon_ui.sprite_request = self.pokemon_ui.sprite_request.wrapping_add(1);
    }
    pub(super) fn load_pokemon_sprite(&mut self, cx: &mut Context<Self>) {
        if self.state.pokemon_disabled {
            return;
        }
        let desired = sprite_identity(&self.state.pokemon);
        let Some(key) = desired else {
            self.cancel_pokemon_sprite();
            self.pokemon_ui.sprite_retry_task.take();
            self.pokemon_ui.sprite_retry_count = 0;
            self.pokemon_ui.sprite_key = None;
            self.pokemon_ui.sprite = None;
            self.refresh_pokemon_animation(cx);
            return;
        };
        if self.pokemon_ui.sprite_key.as_ref() != Some(&key) {
            self.cancel_pokemon_sprite();
            self.pokemon_ui.sprite_retry_task.take();
            self.pokemon_ui.sprite_retry_count = 0;
            self.pokemon_ui.sprite_key = Some(key.clone());
            self.pokemon_ui.sprite = self.pokemon_ui.thumbnails.get(&key).cloned();
            self.refresh_pokemon_animation(cx);
            if let Some(path) = self.pokemon_ui.sprite.as_ref() {
                let decoded = crate::pokemon::sprites::cached_memory(path).is_some()
                    || self
                        .pokemon_ui
                        .footer_asset
                        .as_ref()
                        .is_some_and(|asset| &asset.path == path);
                if !decoded {
                    self.request_pokemon_sprite(key, true, cx);
                } else if !crate::pokemon::sprites::is_exact_animated(
                    path,
                    key.0,
                    key.1,
                    key.2.as_ref(),
                ) {
                    self.schedule_pokemon_sprite_retry(key, cx);
                }
                cx.notify();
                return;
            }
        }
        if self.pokemon_ui.sprite_pending {
            return;
        }
        if let Some(path) = self.pokemon_ui.sprite.as_ref() {
            if crate::pokemon::sprites::cached_memory(path).is_none()
                && !self
                    .pokemon_ui
                    .footer_asset
                    .as_ref()
                    .is_some_and(|asset| &asset.path == path)
            {
                self.request_pokemon_sprite(key, true, cx);
            }
            return;
        }
        self.request_pokemon_sprite(key, false, cx);
    }
    pub(super) fn request_pokemon_sprite(
        &mut self,
        key: SpriteIdentity,
        keep_fallback: bool,
        cx: &mut Context<Self>,
    ) {
        self.cancel_pokemon_sprite();
        self.pokemon_ui.sprite_key = Some(key.clone());
        self.pokemon_ui.sprite_pending = true;
        if !keep_fallback {
            self.pokemon_ui.sprite = None;
            self.refresh_pokemon_animation(cx);
        }
        let generation = self.pokemon_ui.generation;
        let request = self.pokemon_ui.sprite_request;
        let cancel = Arc::new(AtomicBool::new(false));
        self.pokemon_ui.sprite_cancel = Some(cancel.clone());
        #[cfg(test)]
        if !self.usage_live {
            return;
        }
        self.pokemon_ui.sprite_task = Some(cx.spawn(async move |this, cx| {
            let (species, shiny, form) = key.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    if cancel.load(Ordering::SeqCst) {
                        return Err("Sprite request cancelled".into());
                    }
                    api().cached_sprite_form_cancellable(species, shiny, form.as_ref(), cancel)
                })
                .await;
            _ = this.update(cx, |s, cx| {
                s.complete_pokemon_sprite(key, generation, request, result, cx)
            });
        }));
    }
    pub(super) fn complete_pokemon_sprite(
        &mut self,
        key: SpriteIdentity,
        generation: u64,
        request: u64,
        result: Result<PathBuf, String>,
        cx: &mut Context<Self>,
    ) {
        if self.pokemon_ui.generation != generation
            || self.pokemon_ui.sprite_request != request
            || self.state.pokemon_disabled
        {
            return;
        }
        self.pokemon_ui.sprite_pending = false;
        self.pokemon_ui.sprite_cancel = None;
        self.pokemon_ui.sprite_task.take();
        if sprite_identity(&self.state.pokemon).as_ref() != Some(&key) {
            self.load_pokemon_sprite(cx);
            return;
        }
        match result {
            Ok(path) => {
                self.pokemon_ui.thumbnails.insert(key.clone(), path.clone());
                self.pokemon_ui.sprite = Some(path.clone());
                if crate::pokemon::sprites::is_exact_animated(&path, key.0, key.1, key.2.as_ref()) {
                    self.pokemon_ui.sprite_retry_count = 0;
                    self.pokemon_ui.sprite_retry_task.take();
                } else {
                    self.schedule_pokemon_sprite_retry(key, cx);
                }
            }
            Err(error) => {
                self.pokemon_ui.notice = Some(format!("Sprite unavailable: {error}"));
                self.schedule_pokemon_sprite_retry(key, cx);
            }
        }
        self.refresh_pokemon_animation(cx);
        cx.notify();
    }
    fn schedule_pokemon_sprite_retry(&mut self, key: SpriteIdentity, cx: &mut Context<Self>) {
        if self.pokemon_ui.sprite_retry_task.is_some() {
            return;
        }
        let delay = retry_delay(self.pokemon_ui.sprite_retry_count);
        self.pokemon_ui.sprite_retry_count = self.pokemon_ui.sprite_retry_count.saturating_add(1);
        let generation = self.pokemon_ui.generation;
        self.pokemon_ui.sprite_retry_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            _ = this.update(cx, |s, cx| {
                if s.pokemon_ui.generation != generation || s.state.pokemon_disabled {
                    return;
                }
                s.pokemon_ui.sprite_retry_task.take();
                if sprite_identity(&s.state.pokemon).as_ref() == Some(&key) {
                    s.request_pokemon_sprite(key, true, cx);
                }
            });
        }));
    }
    pub(super) fn refresh_pokemon_artwork(&mut self, cx: &mut Context<Self>) {
        self.pokemon_ui.sprite_retry_task.take();
        self.pokemon_ui.sprite_retry_count = 0;
        self.pokemon_ui.thumbnails.retain(|key, path| {
            crate::pokemon::sprites::is_exact_animated(path, key.0, key.1, key.2.as_ref())
        });
        if let Some(key) = sprite_identity(&self.state.pokemon) {
            self.request_pokemon_sprite(key, true, cx);
        }
        self.load_dex_assets(cx);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    // upstream: SpriteAnimationRecoveryTests.swift::testRepeatedFailuresBackOffWithoutAnAttemptLimit
    #[test]
    fn retry_backoff_is_bounded_without_attempt_cutoff() {
        assert_eq!(
            (0..8).map(|i| retry_delay(i).as_secs()).collect::<Vec<_>>(),
            [2, 4, 8, 16, 30, 30, 30, 30]
        );
    }
}

#[cfg(all(test, feature = "test-support"))]
mod native_tests {
    use super::*;
    use crate::app::ui_tests::{UI_LOCK, boot, test_home};
    use core::prelude::v1::test;
    fn companion() -> Companion {
        let mut p = Companion::default();
        p.egg_tokens = Balance::EGG_HATCH_THRESHOLD;
        p.hatch(crate::pokemon::EvoLine {
            base_id: 201,
            tree: crate::pokemon::EvoNode {
                species_id: 201,
                children: vec![],
            },
            rarity: Rarity::Rare,
            names: BTreeMap::new(),
        });
        p.active.as_mut().unwrap().form = Some(crate::pokemon::forms::Form("a".into()));
        p.active.as_mut().unwrap().shiny = false;
        p
    }
    fn key(letter: &str, shiny: bool) -> SpriteIdentity {
        (201, shiny, Some(crate::pokemon::forms::Form(letter.into())))
    }
    // upstream: UnownSpriteViewTests.swift::testCachedAToBToAReturnsToAWhileBLoadIsPending
    // upstream: UnownSpriteViewTests.swift::testCachedNormalToShinyToNormalReturnsToNormalWhileShinyLoadIsPending
    // upstream: SpriteSubjectTests.swift::testCancelledLoadLeavesSubjectUntouched
    // upstream: SpriteSubjectTests.swift::testCancelledLoadArrivingLateCannotRestorePreviousSubject
    // upstream: SpriteSubjectTests.swift::testCancelledFramesArrivingLateCannotResurrectPreviousSpecies
    // upstream: SpriteSubjectTests.swift::testFramesDroppedWhenCancelled
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn late_variant_load_cannot_replace_returned_cached_appearance(cx: &mut TestAppContext) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = test_home();
        let mut state = State::default();
        state.pokemon = companion();
        let (window, shark) = boot(cx, state, WindowOptions::default());
        cx.update_window(window, |_, _, cx| {
            shark.update(cx, |s, cx| {
                for (letter, shiny) in [("b", false), ("a", true)] {
                    let normal = key("a", false);
                    let changing = key(letter, shiny);
                    let path = home.join("sprite-201-normal.gif");
                    s.pokemon_ui.thumbnails.insert(normal.clone(), path.clone());
                    s.pokemon_ui.sprite_key = Some(normal.clone());
                    s.pokemon_ui.sprite = Some(path.clone());
                    let mon = s.state.pokemon.active.as_mut().unwrap();
                    mon.form = changing.2.clone();
                    mon.shiny = shiny;
                    s.load_pokemon_sprite(cx);
                    assert!(
                        s.pokemon_ui.sprite.is_none(),
                        "a different appearance must clear old pixels immediately"
                    );
                    assert!(s.pokemon_ui.sprite_pending);
                    let generation = s.pokemon_ui.generation;
                    let request = s.pokemon_ui.sprite_request;
                    let cancel = s.pokemon_ui.sprite_cancel.clone().unwrap();
                    let mon = s.state.pokemon.active.as_mut().unwrap();
                    mon.form = normal.2.clone();
                    mon.shiny = false;
                    s.load_pokemon_sprite(cx);
                    assert_eq!(s.pokemon_ui.sprite.as_ref(), Some(&path));
                    assert!(cancel.load(Ordering::SeqCst));
                    s.complete_pokemon_sprite(
                        changing,
                        generation,
                        request,
                        Ok(home.join("late.gif")),
                        cx,
                    );
                    assert_eq!(s.pokemon_ui.sprite.as_ref(), Some(&path));
                    assert_eq!(s.pokemon_ui.sprite_key, Some(normal));
                }
            })
        })
        .unwrap();
    }
    // upstream: SpriteSubjectTests.swift::testBecomingEggDropsPreviousSpeciesPixels
    // upstream: SpriteSubjectTests.swift::testBecomingEggWithColdCacheClearsToGlyph
    // upstream: SpriteSubjectTests.swift::testBecomingEggLeavesAnExistingEggAlone
    // upstream: SpriteSubjectTests.swift::testEggImageAppliesOnlyWhenNotCancelled
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn egg_drops_old_pixels_and_late_species_cannot_restore_them(cx: &mut TestAppContext) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = test_home();
        let mut state = State::default();
        state.pokemon = companion();
        let (window, shark) = boot(cx, state, WindowOptions::default());
        cx.update_window(window, |_, _, cx| {
            shark.update(cx, |s, cx| {
                s.pokemon_ui.sprite = Some(home.join("old.gif"));
                s.pokemon_ui.sprite_key = Some(key("a", false));
                s.request_pokemon_sprite(key("a", false), true, cx);
                let generation = s.pokemon_ui.generation;
                let request = s.pokemon_ui.sprite_request;
                s.state.pokemon.active = None;
                s.load_pokemon_sprite(cx);
                assert!(s.pokemon_ui.sprite.is_none());
                assert!(s.pokemon_ui.sprite_key.is_none());
                assert!(!s.pokemon_ui.sprite_pending);
                s.complete_pokemon_sprite(
                    key("a", false),
                    generation,
                    request,
                    Ok(home.join("late.gif")),
                    cx,
                );
                assert!(s.pokemon_ui.sprite.is_none());
                s.load_pokemon_sprite(cx);
                assert!(s.pokemon_ui.sprite.is_none());
                assert!(s.pokemon_ui.sprite_key.is_none());
            })
        })
        .unwrap();
    }
    // upstream: SpriteSubjectTests.swift::testAcceptedLoadUpdatesImageAndID
    // upstream: SpriteSubjectTests.swift::testFailedButNotCancelledLoadStillMarksTheAttempt
    // upstream: SpriteAnimationRecoveryTests.swift::testInitialFailureKeepsFallbackThenRecoversTheSameSubject
    // upstream: SpriteAnimationRecoveryTests.swift::testRetrySeesGIFCachedByAnotherSurface
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn failed_animation_keeps_static_fallback_and_retry_publishes_preferred_variant(
        cx: &mut TestAppContext,
    ) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = test_home();
        let mut state = State::default();
        state.pokemon = companion();
        let (window, shark) = boot(cx, state, WindowOptions::default());
        let fallback = home.join("sprite-201-normal.png");
        cx.update_window(window, |_, _, cx| {
            shark.update(cx, |s, cx| {
                s.pokemon_ui.sprite = Some(fallback.clone());
                s.pokemon_ui.sprite_key = Some(key("a", false));
                s.request_pokemon_sprite(key("a", false), true, cx);
                s.complete_pokemon_sprite(
                    key("a", false),
                    s.pokemon_ui.generation,
                    s.pokemon_ui.sprite_request,
                    Err("offline".into()),
                    cx,
                );
                assert_eq!(s.pokemon_ui.sprite.as_ref(), Some(&fallback));
                assert_eq!(s.pokemon_ui.sprite_key, Some(key("a", false)));
                assert!(s.pokemon_ui.sprite_retry_task.is_some());
                assert!(!s.pokemon_ui.sprite_pending);
            })
        })
        .unwrap();
        cx.executor().advance_clock(Duration::from_secs(2));
        cx.run_until_parked();
        cx.update_window(window, |_, _, cx| {
            shark.update(cx, |s, cx| {
                assert!(s.pokemon_ui.sprite_pending);
                assert_eq!(s.pokemon_ui.sprite.as_ref(), Some(&fallback));
                let preferred = home.join("sprite-201-normal.gif");
                s.complete_pokemon_sprite(
                    key("a", false),
                    s.pokemon_ui.generation,
                    s.pokemon_ui.sprite_request,
                    Ok(preferred.clone()),
                    cx,
                );
                assert_eq!(s.pokemon_ui.sprite, Some(preferred));
                assert!(s.pokemon_ui.sprite_retry_task.is_none());
                assert_eq!(s.pokemon_ui.sprite_retry_count, 0);
            })
        })
        .unwrap();
    }
    // upstream: SpriteAnimationRecoveryTests.swift::testCancellationInterruptsTheProductionBackoff
    // upstream: SpriteAnimationRecoveryTests.swift::testAlreadyCancelledLoadDoesNotRequestOrApplyFallback
    // upstream: SpriteAnimationRecoveryTests.swift::testCancellationWhileApplyingFallbackDoesNotScheduleRetry
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn disabling_cancels_backoff_and_cannot_publish_or_restart_a_load(
        cx: &mut TestAppContext,
    ) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        test_home();
        let mut state = State::default();
        state.pokemon = companion();
        let (window, shark) = boot(cx, state, WindowOptions::default());
        cx.update_window(window, |_, _, cx| {
            shark.update(cx, |s, cx| {
                s.request_pokemon_sprite(key("a", false), false, cx);
                s.complete_pokemon_sprite(
                    key("a", false),
                    s.pokemon_ui.generation,
                    s.pokemon_ui.sprite_request,
                    Err("offline".into()),
                    cx,
                );
                assert!(s.pokemon_ui.sprite_retry_task.is_some());
                s.set_pokemon_enabled(false, cx);
                assert!(s.pokemon_ui.sprite_retry_task.is_none());
                assert!(s.pokemon_ui.sprite_task.is_none());
                assert!(!s.pokemon_ui.sprite_pending);
            })
        })
        .unwrap();
        cx.executor().advance_clock(Duration::from_secs(60));
        cx.run_until_parked();
        cx.update_window(window, |_, _, cx| {
            assert!(!shark.read(cx).pokemon_ui.sprite_pending);
            assert!(shark.read(cx).pokemon_ui.sprite_retry_task.is_none());
        })
        .unwrap();
    }
}
