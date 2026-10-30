//! Images and videos in the diff: a before/after row under the file's header.
//!
//! Both sides are named by the blob hash git prints on the patch's `index` line (git hashes
//! working-tree files too), so every byte copy and preview is cached on disk by content and
//! never goes stale. Previews are capped at `MAX` px: gpui keeps every decoded image at full
//! size, in memory and on the GPU, until it is dropped, and `Lru` drops the oldest ones.
//! Videos play inline through AVFoundation, which hands decoded NV12 frames straight to gpui's
//! `surface()`, one video at a time.
use super::*;
use objc2_av_foundation::AVPlayerItemStatus;
use player::Player;
use prepare::{Preview, ext, playable, touch};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

mod player;
mod prepare;
pub(crate) use prepare::{Kind, MediaFile, prune_cache};

/// Height of the before/after boxes, fixed so a row never jumps when its image loads.
const BOX: f32 = 280.;
/// Decoded images kept; more are dropped oldest first, from memory and the GPU atlas.
const KEEP: usize = 32;

/// An image cache that keeps the `KEEP` most recently drawn images and frees the rest,
/// including their GPU textures. gpui's own caches keep everything forever.
pub(crate) struct Lru(VecDeque<(u64, ImageCacheItem)>);

impl Lru {
    pub(crate) fn new(cx: &mut App) -> Entity<Lru> {
        cx.new(|_| Lru(VecDeque::new()))
    }
}

impl ImageCache for Lru {
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        resource.hash(&mut h);
        let key = h.finish();
        match self.0.iter().position(|(k, _)| *k == key) {
            Some(at) => {
                let item = self.0.remove(at).unwrap();
                self.0.push_back(item);
            }
            None => self.0.push_back((key, ImageCacheItem::new(resource, cx))),
        }
        while self.0.len() > KEEP {
            if let Some((_, item)) = self.0.pop_front()
                && let Some(Ok(image)) = item.get()
            {
                cx.drop_image(image, Some(window));
            }
        }
        self.0.back()?.1.use_image(window)
    }
}

enum Slot {
    Loading,
    Ready(Preview),
    Failed(String),
}

/// The before/after row of one media file.
pub(crate) struct MediaView {
    pub(crate) file: MediaFile,
    slots: [Option<Slot>; 2],
    started: bool,
    cache: Entity<Lru>,
    player: Option<Player>,
    pumping: bool,
    /// Where the row was last drawn, to redraw it on each video frame.
    window: Option<AnyWindowHandle>,
    /// The diff pane's width, measured last frame.
    width: Rc<std::cell::Cell<Pixels>>,
}

impl MediaView {
    pub(crate) fn new(
        file: MediaFile,
        cache: Entity<Lru>,
        width: Rc<std::cell::Cell<Pixels>>,
    ) -> MediaView {
        MediaView {
            width,
            file,
            slots: [None, None],
            started: false,
            cache,
            player: None,
            pumping: false,
            window: None,
        }
    }

    /// Loads both sides off the main thread, the first time the row is drawn.
    fn start(&mut self, cx: &mut Context<Self>) {
        self.started = true;
        let sides = [self.file.old.clone(), self.file.new.clone()];
        for (ix, side) in sides.into_iter().enumerate() {
            let Some(side) = side else { continue };
            self.slots[ix] = Some(Slot::Loading);
            let (path, kind) = (self.file.path.to_string(), self.file.kind);
            cx.spawn(async move |this, cx| {
                let slot = cx
                    .background_executor()
                    .spawn(async move {
                        let p = prepare::prepare(&side, &path, kind);
                        if let Ok(p) = &p {
                            touch(&p.source);
                            p.image.iter().for_each(|i| touch(i));
                        }
                        p
                    })
                    .await;
                this.update(cx, |this, cx| {
                    this.slots[ix] = Some(match slot {
                        Ok(p) => Slot::Ready(p),
                        Err(e) => Slot::Failed(e),
                    });
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    fn preview(&self, side: usize) -> Option<&Preview> {
        match &self.slots[side] {
            Some(Slot::Ready(p)) => Some(p),
            _ => None,
        }
    }

    /// Play/pause: the new side, or the old one when the file was deleted.
    pub(crate) fn toggle_default(&mut self, cx: &mut Context<Self>) {
        let side = if self.file.new.is_some() { 1 } else { 0 };
        self.toggle(side, cx);
    }

    fn toggle(&mut self, side: usize, cx: &mut Context<Self>) {
        let Some(p) = self.preview(side).cloned() else {
            return;
        };
        if self.file.kind == Kind::Image || !playable(&ext(&self.file.path)) {
            return cx.open_with_system(&p.source);
        }
        match &self.player {
            Some(pl) if pl.side == side => pl.toggle(),
            _ => {
                self.player = Player::new(side, &p.source, p.rotated);
                cx.emit(Playing);
            }
        }
        self.pump(cx);
        cx.notify();
    }

    pub(crate) fn stop(&mut self, cx: &mut Context<Self>) {
        if self.player.take().is_some() {
            cx.notify();
        }
    }

    /// Redraws on each new frame, at the video's rate rather than the display's, while it plays
    /// and is on screen.
    fn pump(&mut self, cx: &mut Context<Self>) {
        if self.pumping {
            return;
        }
        self.pumping = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(8))
                    .await;
                let go = this.update(cx, |this, cx| {
                    let window = this.window;
                    let Some(p) = &mut this.player else {
                        this.pumping = false;
                        return (false, None);
                    };
                    // A failed item, or one with no picture (audio only, a codec this Mac can't
                    // decode), would otherwise spin forever.
                    let stuck = p.frame.is_none() && p.started.elapsed() > Duration::from_secs(5);
                    if stuck || unsafe { p.item.status() } == AVPlayerItemStatus::Failed {
                        let side = p.side;
                        this.player = None;
                        this.pumping = false;
                        this.slots[side] = Some(Slot::Failed("can't play it here".into()));
                        cx.notify();
                        return (false, window);
                    }
                    if p.seen.elapsed() > Duration::from_millis(500) {
                        unsafe { p.player.pause() };
                    }
                    let fresh = p.pull();
                    if fresh {
                        cx.notify();
                    }
                    // Draw at least 5 times a second, so `seen` tells on-screen from scrolled
                    // away even when the video's own frames are further apart.
                    let fresh = fresh || p.seen.elapsed() > Duration::from_millis(200);
                    // The first frame can land after a pause.
                    let go = p.playing() || p.frame.is_none();
                    this.pumping = go;
                    (go, window.filter(|_| fresh))
                });
                let Ok((go, redraw)) = go else { break };
                // The diff's list doesn't always draw a notified row again, so the whole
                // window is redrawn; a frame of it costs about a millisecond.
                if let Some(w) = redraw {
                    cx.update_window(w, |_, w, _| w.refresh()).ok();
                }
                if !go {
                    break;
                }
            }
        })
        .detach();
    }

    fn tile(&self, side: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, faint_bg) = (theme.muted_foreground, theme.muted.opacity(0.4));
        let missing = [&self.file.old, &self.file.new][side].is_none();
        let label = match (side, &self.slots[side]) {
            (0, _) if missing => "Before · none".to_string(),
            (_, _) if missing => "After · deleted".to_string(),
            (_, Some(Slot::Ready(p))) => {
                let mut s = vec![
                    if side == 0 { "Before" } else { "After" }.to_string(),
                    ext(&self.file.path).to_uppercase(),
                ];
                if let Some((w, h)) = p.size {
                    s.push(format!("{w}×{h}"));
                }
                s.push(human_bytes(p.bytes));
                s.join(" · ")
            }
            _ => if side == 0 { "Before" } else { "After" }.to_string(),
        };
        let playing = self.player.as_ref().filter(|p| p.side == side);
        let body: AnyElement = match (&self.slots[side], playing) {
            (_, Some(Player { frame: Some(f), .. })) => surface(f.clone())
                .object_fit(ObjectFit::Contain)
                .absolute()
                .inset_0()
                .size_full()
                .into_any_element(),
            (Some(Slot::Loading), _) | (_, Some(_)) => {
                Spinner::new().color(muted).into_any_element()
            }
            (Some(Slot::Failed(e)), _) => div()
                .w_full()
                .px_3()
                .text_xs()
                .text_center()
                .whitespace_normal()
                .child(format!("Can't preview: {e}"))
                .into_any_element(),
            (Some(Slot::Ready(p)), None) => match &p.image {
                Some(image) => div()
                    .relative()
                    .size_full()
                    .child(
                        // Absolute, or the image's own size wins over the box's in flex layout.
                        img(image.clone())
                            .image_cache(&self.cache)
                            .id(("media-img", side))
                            .absolute()
                            .inset_0()
                            .size_full()
                            .object_fit(ObjectFit::ScaleDown)
                            .with_fallback(|| div().child("Can't preview").into_any_element()),
                    )
                    .when(self.file.kind == Kind::Video, |d| {
                        d.child(
                            div()
                                .absolute()
                                .inset_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(play_badge()),
                        )
                    })
                    .into_any_element(),
                None => v_flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .child(Icon::new(IconName::ExternalLink).with_size(px(20.)))
                    .child("No inline preview · click to open")
                    .into_any_element(),
            },
            (None, None) => div().into_any_element(),
        };
        let clickable = matches!(self.slots[side], Some(Slot::Ready(_)));
        v_flex()
            .flex_1()
            .min_w_0()
            .gap_1()
            .child(div().text_xs().text_color(muted).truncate().child(label))
            .child(
                div()
                    .id(("media-tile", side))
                    .h(px(BOX))
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .overflow_hidden()
                    .rounded(theme.radius)
                    .bg(faint_bg)
                    .text_color(muted)
                    .when(clickable, |d| {
                        d.cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| this.toggle(side, cx)))
                    })
                    .child(body),
            )
            .into_any_element()
    }
}

fn play_badge() -> impl IntoElement {
    div()
        .size(px(44.))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui_kit::black().opacity(0.55))
        .text_color(gpui_kit::white())
        .child(Icon::new(IconName::Play).with_size(px(20.)))
}

fn human_bytes(n: u64) -> String {
    match n {
        n if n < 1024 => format!("{n} B"),
        n if n < 1024 * 1024 => format!("{:.1} KB", n as f64 / 1024.),
        n => format!("{:.1} MB", n as f64 / (1024. * 1024.)),
    }
}

/// A video started; the others stop, so one decoder runs at a time.
pub(crate) struct Playing;
impl EventEmitter<Playing> for MediaView {}

impl Render for MediaView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.window = Some(window.window_handle());
        if !self.started {
            self.start(cx);
        }
        if let Some(p) = &mut self.player {
            p.seen = Instant::now();
        }
        // The diff is as wide as its longest line; the row stays as wide as the pane (less the
        // annotation's padding), so After isn't pushed out of view.
        let width = self.width.get() - px(24.);
        h_flex()
            .map(|d| {
                if width > px(0.) {
                    d.w(width)
                } else {
                    d.w_full()
                }
            })
            .gap_3()
            .items_start()
            .child(self.tile(0, cx))
            .child(self.tile(1, cx))
    }
}

#[cfg(test)]
impl MediaView {
    pub(crate) fn started(&self) -> bool {
        self.started
    }

    /// Every side the file has is loaded (or failed).
    pub(crate) fn ready(&self) -> bool {
        let has = [self.file.old.is_some(), self.file.new.is_some()];
        (0..2).all(|i| !has[i] || matches!(self.slots[i], Some(Slot::Ready(_) | Slot::Failed(_))))
    }

    pub(crate) fn failed(&self, side: usize) -> Option<String> {
        match &self.slots[side] {
            Some(Slot::Failed(e)) => Some(e.clone()),
            _ => None,
        }
    }

    /// A loaded side: what is drawn, and the source's size.
    #[allow(clippy::type_complexity)]
    pub(crate) fn shown(&self, side: usize) -> Option<(Option<PathBuf>, Option<(usize, usize)>)> {
        match &self.slots[side] {
            Some(Slot::Failed(e)) => panic!("{}: {e}", self.file.path),
            _ => self.preview(side).map(|p| (p.image.clone(), p.size)),
        }
    }
}
