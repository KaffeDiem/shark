//! Images and videos shared by Diff and Git: a before/after row, or one side alone.
//!
//! Each side names where its bytes come from: the working tree, an exact Git blob, or a
//! downloaded file. Blob sides are named by their hash (git hashes working-tree files too), so
//! every byte copy and preview is cached on disk by content and never goes stale. Previews are capped at `MAX` px: gpui keeps every decoded image at full
//! size, in memory and on the GPU, until it is dropped, and `Lru` drops the oldest ones.
//! Videos play inline through AVFoundation, which hands decoded NV12 frames straight to gpui's
//! `surface()`, one video at a time.
use gpui_kit::assets::IconName;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use objc2::AnyThread as _;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_av_foundation::{
    AVMediaTypeVideo, AVPlayer, AVPlayerItem, AVPlayerItemStatus, AVPlayerItemVideoOutput,
    AVURLAsset, AVVideoComposition,
};
use objc2_core_foundation::{CFRetained, CFString};
use objc2_core_video::{
    CVPixelBuffer, kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
};
use objc2_foundation::{MainThreadMarker, NSDictionary, NSNumber, NSString, NSURL};
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(crate) mod fetch;

/// Longest side of a preview in px: about a full-width side on a Retina screen.
const MAX: usize = 1200;
/// Height of the before/after boxes, fixed so a row never jumps when its image loads.
const BOX: f32 = 280.;
/// Decoded images kept; more are dropped oldest first, from memory and the GPU atlas.
const KEEP: usize = 32;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Kind {
    Image,
    Video,
}

fn ext(path: &str) -> String {
    Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

pub(crate) fn kind(path: &str) -> Option<Kind> {
    match ext(path).as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "svg" | "tif" | "tiff"
        | "heic" | "heif" | "avif" | "psd" => Some(Kind::Image),
        "mp4" | "mov" | "m4v" | "webm" | "mkv" | "avi" => Some(Kind::Video),
        _ => None,
    }
}

/// What gpui decodes itself; the rest goes through `sips` (ImageIO) first.
fn native(ext: &str) -> bool {
    matches!(
        ext,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "svg"
    )
}

/// AVFoundation can't open WebM, MKV or AVI; those only get an Open button.
fn playable(ext: &str) -> bool {
    matches!(ext, "mp4" | "mov" | "m4v")
}

/// Where one side's bytes come from.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Source {
    /// A working-tree file, which must still hash to `Side::sha`.
    Working(PathBuf),
    /// The exact blob `Side::sha`, read from the repository.
    Blob,
    /// A file already downloaded into the cache; `Side::sha` is its cache key.
    Downloaded(PathBuf),
}

/// One side of a media file.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Side {
    pub(crate) sha: String,
    /// The worktree, for `git cat-file`.
    pub(crate) repo: PathBuf,
    pub(crate) source: Source,
}

impl Side {
    /// Cache name. A working file can hash to a blob while holding other bytes (Git LFS and
    /// other clean filters), so working copies and raw blobs never share an entry.
    fn key(&self) -> String {
        match self.source {
            Source::Working(_) => format!("w-{}", self.sha),
            Source::Blob => format!("b-{}", self.sha),
            Source::Downloaded(_) => self.sha.clone(),
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub(crate) struct MediaFile {
    pub(crate) path: SharedString,
    pub(crate) kind: Kind,
    pub(crate) old: Option<Side>,
    pub(crate) new: Option<Side>,
}

impl MediaFile {
    /// From one file's patch, whose paths are `<repo>/<path>` under `ws_dir`.
    pub(crate) fn parse(path: &str, chunk: &str, ws_dir: &Path) -> Option<MediaFile> {
        let kind = kind(path)?;
        let (old, new) = chunk
            .lines()
            .take_while(|l| !l.starts_with("@@"))
            .find_map(|l| l.strip_prefix("index "))?
            .split(' ')
            .next()?
            .split_once("..")?;
        let repo = ws_dir.join(path.split('/').next()?);
        let side = |sha: &str, source| {
            (!sha.bytes().all(|b| b == b'0')).then(|| Side {
                sha: sha.to_string(),
                repo: repo.clone(),
                source,
            })
        };
        Some(MediaFile {
            path: path.to_string().into(),
            kind,
            old: side(old, Source::Blob),
            new: side(new, Source::Working(ws_dir.join(path))),
        })
    }
}

/// A side ready to show.
#[derive(Clone, Debug)]
pub(crate) struct Preview {
    /// The full-size copy, for playing and opening.
    source: PathBuf,
    /// What is drawn: the source itself, a downscaled PNG or a video's poster frame.
    image: Option<PathBuf>,
    size: Option<(usize, usize)>,
    bytes: u64,
    /// A video stored sideways with a rotation to apply (phones record portrait this way).
    rotated: bool,
}

pub(crate) fn cache_dir() -> PathBuf {
    crate::state::home().join("Library/Caches/shark/media")
}

/// Writes `out` through a temp file, so a crash or a concurrent load never leaves half a file.
pub(crate) fn produce(
    out: &Path,
    make: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    if out.exists() {
        return Ok(());
    }
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = out.file_name().unwrap_or_default().to_string_lossy();
    let tmp = out.with_file_name(format!(".{}-{seq}-{name}", std::process::id()));
    let made = make(&tmp).and_then(|_| fs::rename(&tmp, out).map_err(|e| e.to_string()));
    let _ = fs::remove_file(&tmp);
    made
}

fn run(cmd: &mut Command) -> Result<std::process::Output, String> {
    let o = cmd.output().map_err(|e| e.to_string())?;
    match o.status.success() {
        true => Ok(o),
        // sips adds a second line with a generic code; the first one says what went wrong.
        false => Err((String::from_utf8_lossy(&o.stderr).lines().next())
            .unwrap_or("failed")
            .trim()
            .trim_start_matches("Error: ")
            .to_string()),
    }
}

/// Copies the side's bytes into the cache and makes its preview. Blocking: background only.
pub(crate) fn prepare(side: &Side, path: &str, kind: Kind) -> Result<Preview, String> {
    let dir = cache_dir();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let ext = ext(path);
    let key = side.key();
    let source = dir.join(format!("{key}.{ext}"));
    produce(&source, |tmp| match &side.source {
        // The row loads after the diff ran; a file rewritten in between must not be cached
        // under the older hash. `--path` applies the same filters git diff hashed with.
        Source::Working(f) => {
            fs::copy(f, tmp).map_err(|e| e.to_string())?;
            let rel = path.split_once('/').map_or(path, |(_, r)| r);
            let o = run(Command::new("git")
                .current_dir(&side.repo)
                .args(["hash-object", &format!("--path={rel}")])
                .arg(tmp))?;
            match String::from_utf8_lossy(&o.stdout).trim() == side.sha {
                true => Ok(()),
                false => Err("changed on disk since the diff; refresh (r)".into()),
            }
        }
        // Raw bytes, bounded: a historical side never comes from the working tree.
        Source::Blob => {
            let bytes = crate::git::history::blob(&side.repo, &side.sha)?;
            fs::write(tmp, bytes).map_err(|e| e.to_string())
        }
        Source::Downloaded(f) => fs::copy(f, tmp).map(|_| ()).map_err(|e| e.to_string()),
    })?;
    let bytes = fs::metadata(&source).map_err(|e| e.to_string())?.len();
    if bytes == 0 {
        return Err("empty file".into());
    }
    if bytes < 1024 && fs::read(&source).is_ok_and(|b| b.starts_with(b"version https://git-lfs")) {
        return Err("a Git LFS pointer; the file itself isn't checked out".into());
    }
    // gpui draws an SVG at its own size, so a big one is rendered down here first.
    let svg = (ext == "svg")
        .then(|| {
            let data = fs::read(&source).map_err(|e| e.to_string())?;
            resvg::usvg::Tree::from_data(&data, &Default::default()).map_err(|e| e.to_string())
        })
        .transpose()?;
    let video = (kind == Kind::Video && playable(&ext))
        .then(|| video_track(&source))
        .flatten();
    let size = match (&svg, video) {
        (Some(t), _) => Some((
            t.size().width().ceil() as usize,
            t.size().height().ceil() as usize,
        )),
        (_, Some((size, _))) => Some(size),
        _ if kind == Kind::Video => None,
        _ => imagesize::size(&source).ok().map(|s| (s.width, s.height)),
    };
    let big = size.is_some_and(|(w, h)| w.max(h) > MAX);
    let image = match kind {
        Kind::Image if let Some(tree) = svg.filter(|_| big) => {
            let out = dir.join(format!("{key}.preview.png"));
            produce(&out, |tmp| {
                let scale = MAX as f32 / tree.size().width().max(tree.size().height());
                let (w, h) = (tree.size().width() * scale, tree.size().height() * scale);
                let mut pixmap = resvg::tiny_skia::Pixmap::new(w.ceil() as u32, h.ceil() as u32)
                    .ok_or("SVG too small to draw")?;
                let at = resvg::tiny_skia::Transform::from_scale(scale, scale);
                resvg::render(&tree, at, &mut pixmap.as_mut());
                pixmap.save_png(tmp).map_err(|e| e.to_string())
            })?;
            Some(out)
        }
        Kind::Image if native(&ext) && !big => Some(source.clone()),
        Kind::Image => {
            let out = dir.join(format!("{key}.preview.png"));
            produce(&out, |tmp| {
                let mut sips = Command::new("sips");
                if big {
                    sips.args(["-Z", &MAX.to_string()]);
                }
                run(sips
                    .args(["-s", "format", "png"])
                    .arg(&source)
                    .arg("--out")
                    .arg(tmp))
                .map(|_| ())
            })?;
            Some(out)
        }
        Kind::Video if playable(&ext) => {
            let out = dir.join(format!("{key}.poster.png"));
            // Quick Look's thumbnailer decodes one frame; it hangs on broken files, so it gets
            // a deadline, and a failure is remembered so it costs that wait once, not per launch.
            let failed = dir.join(format!("{key}.poster.none"));
            (!failed.exists())
                .then(|| {
                    produce(&out, |tmp| {
                        let tmp_dir = tmp.with_extension("d");
                        fs::create_dir_all(&tmp_dir).map_err(|e| e.to_string())?;
                        let made = poster(&source, &tmp_dir)
                            .and_then(|png| fs::rename(png, tmp).map_err(|e| e.to_string()));
                        let _ = fs::remove_dir_all(&tmp_dir);
                        made
                    })
                    .inspect_err(|_| {
                        let _ = fs::write(&failed, "");
                    })
                    .ok()
                    .map(|_| out)
                })
                .flatten()
        }
        Kind::Video => None,
    };
    Ok(Preview {
        source,
        image,
        size,
        bytes,
        rotated: video.is_some_and(|(_, r)| r),
    })
}

/// The first video track's upright size, and whether it is stored rotated. Loads the track
/// table synchronously, so background only.
fn video_track(file: &Path) -> Option<((usize, usize), bool)> {
    unsafe {
        let url = NSURL::fileURLWithPath(&NSString::from_str(&file.to_string_lossy()));
        let asset = AVURLAsset::URLAssetWithURL_options(&url, None);
        #[allow(deprecated)]
        let track = asset.tracksWithMediaType(AVMediaTypeVideo?).firstObject()?;
        let (n, t) = (track.naturalSize(), track.preferredTransform());
        let quarter = t.b != 0. || t.c != 0.;
        let (w, h) = if quarter {
            (n.height, n.width)
        } else {
            (n.width, n.height)
        };
        let rotated = quarter || t.a < 0. || t.d < 0.;
        Some(((w.abs() as usize, h.abs() as usize), rotated))
    }
}

fn poster(video: &Path, out_dir: &Path) -> Result<PathBuf, String> {
    let mut child = Command::new("qlmanage")
        .args(["-t", "-s", &MAX.to_string(), "-o"])
        .arg(out_dir)
        .arg(video)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().map_err(|e| e.to_string())?.is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("qlmanage timed out".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let name = format!(
        "{}.png",
        video.file_name().unwrap_or_default().to_string_lossy()
    );
    let png = out_dir.join(name);
    match png.exists() {
        true => Ok(png),
        false => Err("no poster".into()),
    }
}

/// Deletes cached copies not used for two weeks. Each load touches what it uses.
pub(crate) fn prune_cache() {
    let Ok(entries) = fs::read_dir(cache_dir()) else {
        return;
    };
    let old = std::time::SystemTime::now() - Duration::from_secs(14 * 24 * 3600);
    for e in entries.flatten() {
        let stale = (e.metadata().and_then(|m| m.modified())).is_ok_and(|t| t < old);
        if stale {
            let _ = fs::remove_file(e.path());
        }
    }
}

fn touch(path: &Path) {
    if let Ok(f) = fs::File::options().append(true).open(path) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
}

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

/// The video being played, and its newest frame.
struct Player {
    side: usize,
    player: Retained<AVPlayer>,
    item: Retained<AVPlayerItem>,
    out: Retained<AVPlayerItemVideoOutput>,
    frame: Option<CFRetained<CVPixelBuffer>>,
    /// Last time the tile was drawn; scrolled out of view, the video pauses.
    seen: Instant,
    started: Instant,
}

impl Player {
    fn new(side: usize, file: &Path, rotated: bool) -> Option<Player> {
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

    fn playing(&self) -> bool {
        unsafe { self.player.rate() > 0. }
    }

    fn toggle(&self) {
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
    fn pull(&mut self) -> bool {
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
    /// Shows one side alone, under this label (a historical Before/After or a document image).
    only: Option<(usize, SharedString)>,
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
            only: None,
        }
    }

    pub(crate) fn only(mut self, side: usize, label: impl Into<SharedString>) -> MediaView {
        self.only = Some((side, label.into()));
        self
    }

    /// Loads both sides off the main thread, the first time the row is drawn.
    fn start(&mut self, cx: &mut Context<Self>) {
        self.started = true;
        let sides = [self.file.old.clone(), self.file.new.clone()];
        for (ix, side) in sides.into_iter().enumerate() {
            let Some(side) = side else { continue };
            if self.only.as_ref().is_some_and(|(only, _)| *only != ix) {
                continue;
            }
            self.slots[ix] = Some(Slot::Loading);
            let (path, kind) = (self.file.path.to_string(), self.file.kind);
            cx.spawn(async move |this, cx| {
                let slot = cx
                    .background_executor()
                    .spawn(async move {
                        let p = prepare(&side, &path, kind);
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
        let side = match &self.only {
            Some((side, _)) => *side,
            None if self.file.new.is_some() => 1,
            None => 0,
        };
        self.toggle(side, cx);
    }

    fn toggle(&mut self, side: usize, cx: &mut Context<Self>) {
        let Some(p) = self.preview(side).cloned() else {
            return;
        };
        if self.file.kind == Kind::Image || !playable(&ext(&self.file.path)) {
            return open(&p.source);
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
        let name = match &self.only {
            Some((_, label)) => label.to_string(),
            None => if side == 0 { "Before" } else { "After" }.to_string(),
        };
        let label = match (side, &self.slots[side]) {
            _ if missing && self.only.is_some() => format!("{name} · none"),
            (0, _) if missing => "Before · none".to_string(),
            (_, _) if missing => "After · deleted".to_string(),
            (_, Some(Slot::Ready(p))) => {
                let mut s = vec![name, ext(&self.file.path).to_uppercase()];
                if let Some((w, h)) = p.size {
                    s.push(format!("{w}×{h}"));
                }
                s.push(human_bytes(p.bytes));
                s.join(" · ")
            }
            _ => name,
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

fn open(path: &Path) {
    let _ = Command::new("open").arg(path).spawn();
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
            .map(|d| match self.only.as_ref().map(|(side, _)| *side) {
                Some(side) => d.child(self.tile(side, cx)),
                None => d.child(self.tile(0, cx)).child(self.tile(1, cx)),
            })
    }
}

#[cfg(test)]
impl MediaView {
    pub(crate) fn started(&self) -> bool {
        self.started
    }

    /// Every side the file has is loaded (or failed).
    pub(crate) fn ready(&self) -> bool {
        let mut has = [self.file.old.is_some(), self.file.new.is_some()];
        if let Some((side, _)) = &self.only {
            has[1 - side] = false;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn parses_index_line() {
        let chunk = "diff --git a/web/a.png b/web/a.png\n\
                     index 667fa07ce3c6cf443edfed540420eb8968684eea..0000000000000000000000000000000000000000 100644\n\
                     Binary files a/web/a.png and /dev/null differ\n";
        let m = MediaFile::parse("web/a.png", chunk, Path::new("/ws")).unwrap();
        assert_eq!(m.kind, Kind::Image);
        let old = m.old.unwrap();
        assert_eq!(old.sha, "667fa07ce3c6cf443edfed540420eb8968684eea");
        assert_eq!(old.repo, Path::new("/ws/web"));
        assert!(m.new.is_none(), "all-zero hash is a missing side");
        assert!(MediaFile::parse("web/a.rs", chunk, Path::new("/ws")).is_none());
        assert_eq!(kind("x/Clip.MOV"), Some(Kind::Video));
        let new = m.new.clone().unwrap_or(Side {
            sha: old.sha.clone(),
            repo: old.repo.clone(),
            source: Source::Working("/ws/web/a.png".into()),
        });
        assert_ne!(
            new.key(),
            old.key(),
            "working copies never reuse a blob's cache entry"
        );
    }
}
