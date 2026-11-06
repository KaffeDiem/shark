//! The AVFoundation player of one video, and its newest decoded frame.
use objc2::AnyThread as _;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_av_foundation::{AVPlayer, AVPlayerItem, AVPlayerItemVideoOutput, AVVideoComposition};
use objc2_core_foundation::{CFRetained, CFString};
use objc2_core_video::{
    CVPixelBuffer, kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
};
use objc2_foundation::{MainThreadMarker, NSDictionary, NSNumber, NSString, NSURL};
use std::path::Path;
use std::time::Instant;

/// The video being played, and its newest frame.
pub(super) struct Player {
    pub(super) side: usize,
    pub(super) player: Retained<AVPlayer>,
    pub(super) item: Retained<AVPlayerItem>,
    out: Retained<AVPlayerItemVideoOutput>,
    pub(super) frame: Option<CFRetained<CVPixelBuffer>>,
    /// Last time the tile was drawn; scrolled out of view, the video pauses.
    pub(super) seen: Instant,
    pub(super) started: Instant,
}

impl Player {
    pub(super) fn new(side: usize, file: &Path, rotated: bool) -> Option<Player> {
        let mtm = MainThreadMarker::new()?;
        let key = |k: &'static CFString| -> &'static NSString {
            // CFString and NSString are toll-free bridged.
            unsafe { &*(k as *const CFString as *const NSString) }
        };
        let format = NSNumber::new_u32(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange);
        let yes = NSNumber::new_bool(true);
        let none = NSDictionary::<NSString, AnyObject>::new();
        unsafe {
            // gpui's renderer takes NV12 full range only; IOSurface-backed buffers go to Metal
            // without a copy.
            let attrs = NSDictionary::<NSString, AnyObject>::from_slices(
                &[
                    key(kCVPixelBufferPixelFormatTypeKey),
                    key(kCVPixelBufferMetalCompatibilityKey),
                    key(kCVPixelBufferIOSurfacePropertiesKey),
                ],
                &[&*format as &AnyObject, &*yes, &*none],
            );
            let out = AVPlayerItemVideoOutput::initWithPixelBufferAttributes(
                AVPlayerItemVideoOutput::alloc(),
                Some(&attrs),
            );
            let url = NSURL::fileURLWithPath(&NSString::from_str(&file.to_string_lossy()));
            let item = AVPlayerItem::playerItemWithURL(&url, mtm);
            // The output hands frames over as stored; a composition turns them upright, at the
            // cost of a GPU pass, so only for videos that need it. It reads the (local) track
            // table synchronously.
            if rotated {
                #[allow(deprecated)]
                let upright =
                    AVVideoComposition::videoCompositionWithPropertiesOfAsset(&item.asset());
                item.setVideoComposition(Some(&upright));
            }
            item.addOutput(&out);
            let player = AVPlayer::playerWithPlayerItem(Some(&item), mtm);
            player.setMuted(true);
            player.play();
            Some(Player {
                side,
                player,
                item,
                out,
                frame: None,
                seen: Instant::now(),
                started: Instant::now(),
            })
        }
    }

    pub(super) fn playing(&self) -> bool {
        unsafe { self.player.rate() > 0. }
    }

    pub(super) fn toggle(&self) {
        unsafe {
            if self.playing() {
                return self.player.pause();
            }
            let (now, end) = (self.item.currentTime(), self.item.duration());
            if end.flags.contains(objc2_core_media::CMTimeFlags::Valid)
                && now.value as f64 / now.timescale.max(1) as f64
                    >= end.value as f64 / end.timescale.max(1) as f64
            {
                self.player.seekToTime(objc2_core_media::kCMTimeZero);
            }
            self.player.play();
        }
    }

    /// Takes the frame due now, if it is new.
    pub(super) fn pull(&mut self) -> bool {
        unsafe {
            let t = self.item.currentTime();
            if !self.out.hasNewPixelBufferForItemTime(t) {
                return false;
            }
            let Some(buf) = self
                .out
                .copyPixelBufferForItemTime_itemTimeForDisplay(t, std::ptr::null_mut())
            else {
                return false;
            };
            // Same +1 reference, other smart pointer: gpui's surface takes a CFRetained.
            let raw = std::ptr::NonNull::new_unchecked(Retained::into_raw(buf));
            self.frame = Some(CFRetained::from_raw(raw));
            true
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        unsafe { self.player.pause() }
    }
}
