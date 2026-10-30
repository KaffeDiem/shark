//! Producing the previews of a media file's sides: byte copies and downscaled images in an
//! on-disk cache keyed by blob hash.
use gpui_kit::SharedString;
use objc2_av_foundation::{AVMediaTypeVideo, AVURLAsset};
use objc2_foundation::{NSString, NSURL};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// Longest side of a preview in px: about a full-width side on a Retina screen.
const MAX: usize = 1200;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Kind {
    Image,
    Video,
}

pub(super) fn ext(path: &str) -> String {
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
pub(super) fn playable(ext: &str) -> bool {
    matches!(ext, "mp4" | "mov" | "m4v")
}

/// One side of a changed media file.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Side {
    pub(crate) sha: String,
    /// The worktree, for `git cat-file`.
    repo: PathBuf,
    /// The working-tree file (the new side); `None` reads the blob from git (the old side).
    file: Option<PathBuf>,
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
        let side = |sha: &str, file| {
            (!sha.bytes().all(|b| b == b'0')).then(|| Side {
                sha: sha.to_string(),
                repo: repo.clone(),
                file,
            })
        };
        Some(MediaFile {
            path: path.to_string().into(),
            kind,
            old: side(old, None),
            new: side(new, Some(ws_dir.join(path))),
        })
    }
}

/// A side ready to show.
#[derive(Clone, Debug)]
pub(crate) struct Preview {
    /// The full-size copy, for playing and opening.
    pub(super) source: PathBuf,
    /// What is drawn: the source itself, a downscaled PNG or a video's poster frame.
    pub(super) image: Option<PathBuf>,
    pub(super) size: Option<(usize, usize)>,
    pub(super) bytes: u64,
    /// A video stored sideways with a rotation to apply (phones record portrait this way).
    pub(super) rotated: bool,
}

pub(crate) fn cache_dir() -> PathBuf {
    crate::state::home().join("Library/Caches/shark/media")
}

/// Writes `out` through a temp file, so a crash or a concurrent load never leaves half a file.
fn produce(out: &Path, make: impl FnOnce(&Path) -> Result<(), String>) -> Result<(), String> {
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
    let source = dir.join(format!("{}.{ext}", side.sha));
    produce(&source, |tmp| match &side.file {
        // The row loads after the diff ran; a file rewritten in between must not be cached
        // under the older hash. `--path` applies the same filters git diff hashed with.
        Some(f) => {
            fs::copy(f, tmp).map_err(|e| e.to_string())?;
            let rel = path.split_once('/').map_or(path, |(_, r)| r);
            let hash = crate::git::git(
                &side.repo,
                &[
                    "hash-object",
                    &format!("--path={rel}"),
                    &tmp.to_string_lossy(),
                ],
            )?;
            match hash == side.sha {
                true => Ok(()),
                false => Err("changed on disk since the diff; refresh (r)".into()),
            }
        }
        None => {
            let blob = crate::git::git_bytes(&side.repo, &["cat-file", "blob", &side.sha])?;
            fs::write(tmp, blob).map_err(|e| e.to_string())
        }
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
            let out = dir.join(format!("{}.preview.png", side.sha));
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
            let out = dir.join(format!("{}.preview.png", side.sha));
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
            let out = dir.join(format!("{}.poster.png", side.sha));
            // Quick Look's thumbnailer decodes one frame; it hangs on broken files, so it gets
            // a deadline, and a failure is remembered so it costs that wait once, not per launch.
            let failed = dir.join(format!("{}.poster.none", side.sha));
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

pub(super) fn touch(path: &Path) {
    if let Ok(f) = fs::File::options().append(true).open(path) {
        let _ = f.set_modified(std::time::SystemTime::now());
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
    }
}
