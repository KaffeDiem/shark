//! Validated, bounded sprite cache. Fallbacks retain their own identities so later artwork wins.
use super::forms::{self, Form};
use image::{AnimationDecoder, Frame, ImageFormat, ImageReader};
use std::{
    collections::BTreeMap,
    fs,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};
const MAX_FILE: usize = 8 * 1024 * 1024;
const CACHE_BUDGET: usize = 64 * 1024 * 1024;
#[derive(Clone, Debug)]
pub struct Request {
    species: u16,
    animated: bool,
    shiny: bool,
    form: Option<Form>,
    item: Option<String>,
}
impl Request {
    pub fn pokemon(species: u16, animated: bool, shiny: bool, form: Option<&Form>) -> Self {
        Self {
            species,
            animated,
            shiny,
            form: forms::resolved(species, form),
            item: None,
        }
    }
    pub fn item(name: &str) -> Self {
        Self {
            species: 0,
            animated: false,
            shiny: false,
            form: None,
            item: Some(name.into()),
        }
    }
    pub fn filename(&self) -> String {
        if let Some(name) = &self.item {
            return format!("item-{name}.png");
        }
        format!(
            "sprite-{}-{}.{}",
            forms::asset_name(self.species, self.form.as_ref()),
            if self.shiny { "shiny" } else { "normal" },
            if self.animated { "gif" } else { "png" }
        )
    }
    fn valid(&self) -> bool {
        if let Some(name) = &self.item {
            !name.is_empty()
                && name.len() < 80
                && name
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        } else {
            (1..=649).contains(&self.species)
        }
    }
    pub fn url(&self) -> String {
        if let Some(name) = &self.item {
            format!(
                "https://raw.githubusercontent.com/PokeAPI/sprites/master/sprites/items/{name}.png"
            )
        } else {
            forms::sprite_url(self.species, self.animated, self.shiny, self.form.as_ref())
        }
    }
    fn candidates(&self) -> Vec<Self> {
        let mut result = vec![self.clone()];
        if self.item.is_some() {
            return result;
        }
        if self.shiny {
            let mut normal = self.clone();
            normal.shiny = false;
            result.push(normal);
        }
        if self.animated {
            let mut static_one = self.clone();
            static_one.animated = false;
            result.push(static_one.clone());
            if static_one.shiny {
                static_one.shiny = false;
                result.push(static_one);
            }
        }
        result
    }
}
pub fn is_exact_animated(path: &Path, species: u16, shiny: bool, form: Option<&Form>) -> bool {
    path.file_name().is_some_and(|n| {
        n == Request::pokemon(species, true, shiny, form)
            .filename()
            .as_str()
    })
}
/// Decoded RGBA frames; GPUI adapters swap R/B once when making their RenderImage.
pub struct Asset {
    pub path: PathBuf,
    pub frames: Vec<Frame>,
    pub encoded: Arc<Vec<u8>>,
}
impl Asset {
    pub fn dimensions(&self) -> (u32, u32) {
        self.frames[0].buffer().dimensions()
    }
    pub fn byte_size(&self) -> usize {
        self.encoded.len() + self.frames.iter().map(|f| f.buffer().len()).sum::<usize>()
    }
}
#[derive(Default)]
struct Memory {
    tick: u64,
    entries: BTreeMap<PathBuf, (u64, Arc<Asset>)>,
    bytes: usize,
}
fn memory() -> &'static Mutex<Memory> {
    static MEMORY: OnceLock<Mutex<Memory>> = OnceLock::new();
    MEMORY.get_or_init(|| Mutex::new(Memory::default()))
}
fn key(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| {
        path.parent()
            .and_then(|p| fs::canonicalize(p).ok())
            .map(|p| p.join(path.file_name().unwrap_or_default()))
            .unwrap_or_else(|| path.to_path_buf())
    })
}
fn memory_get(path: &Path) -> Option<Arc<Asset>> {
    let mut m = memory().lock().unwrap_or_else(|e| e.into_inner());
    m.tick = m.tick.wrapping_add(1);
    let tick = m.tick;
    m.entries.get_mut(path).map(|(seen, asset)| {
        *seen = tick;
        asset.clone()
    })
}
fn remember(asset: Asset) -> Arc<Asset> {
    let asset = Arc::new(asset);
    let mut m = memory().lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, existing)) = m.entries.get(&asset.path) {
        return existing.clone();
    }
    let size = asset.byte_size();
    if size > CACHE_BUDGET {
        return asset;
    }
    while m.bytes + size > CACHE_BUDGET || m.entries.len() >= 256 {
        let Some(old) = m
            .entries
            .iter()
            .min_by_key(|(_, v)| v.0)
            .map(|(k, _)| k.clone())
        else {
            break;
        };
        if let Some((_, a)) = m.entries.remove(&old) {
            m.bytes = m.bytes.saturating_sub(a.byte_size())
        }
    }
    m.tick = m.tick.wrapping_add(1);
    let tick = m.tick;
    m.bytes += size;
    m.entries.insert(asset.path.clone(), (tick, asset.clone()));
    asset
}
pub(super) fn decode(path: PathBuf, bytes: Vec<u8>) -> Result<Asset, String> {
    if bytes.len() > MAX_FILE {
        return Err("Sprite exceeds the file size limit".into());
    }
    let format = image::guess_format(&bytes).map_err(|_| "Invalid Pokémon image")?;
    if (path.extension().is_some_and(|x| x == "gif") && format != ImageFormat::Gif)
        || (path.extension().is_some_and(|x| x == "png") && format != ImageFormat::Png)
    {
        return Err("Image format does not match its sprite variant".into());
    }
    let frames = match format {
        ImageFormat::Gif => {
            if !super::api::valid_gif(&bytes) {
                return Err("Invalid Pokémon animation".into());
            }
            image::codecs::gif::GifDecoder::new(Cursor::new(&bytes))
                .map_err(|_| "Invalid Pokémon animation")?
                .into_frames()
                .collect_frames()
                .map_err(|_| "Invalid Pokémon animation")?
        }
        ImageFormat::Png => {
            let mut reader = ImageReader::with_format(Cursor::new(&bytes), ImageFormat::Png);
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(1024);
            limits.max_image_height = Some(1024);
            limits.max_alloc = Some(32 * 1024 * 1024);
            reader.limits(limits);
            let frame = reader
                .decode()
                .map_err(|_| "Invalid Pokémon image")?
                .into_rgba8();
            if frame.width() == 0 || frame.height() == 0 {
                return Err("Empty Pokémon image".into());
            }
            vec![Frame::new(frame)]
        }
        _ => return Err("Unsupported Pokémon image format".into()),
    };
    let frames = if format == ImageFormat::Gif {
        frames
            .into_iter()
            .map(|frame| {
                let (numerator, denominator) = frame.delay().numer_denom_ms();
                if numerator < denominator.saturating_mul(20) {
                    let (left, top) = (frame.left(), frame.top());
                    Frame::from_parts(
                        frame.into_buffer(),
                        left,
                        top,
                        image::Delay::from_numer_denom_ms(100, 1),
                    )
                } else {
                    frame
                }
            })
            .collect::<Vec<_>>()
    } else {
        frames
    };
    if frames.is_empty()
        || frames.iter().map(|f| f.buffer().len()).sum::<usize>() > 32 * 1024 * 1024
    {
        return Err("Animation exceeds the frame memory limit".into());
    }
    Ok(Asset {
        path,
        frames,
        encoded: Arc::new(bytes),
    })
}
/// No filesystem work: renderers use paths returned by load_with, which are already canonical.
pub fn cached_memory(path: &Path) -> Option<Arc<Asset>> {
    memory_get(path)
}

/// Warm lookups retain the same decoded frames even when the file has disappeared.
pub fn cached(path: &Path) -> Option<Arc<Asset>> {
    cached_checked(path, || true)
}
fn cached_checked(path: &Path, allowed: impl Fn() -> bool) -> Option<Arc<Asset>> {
    if !allowed() {
        return None;
    }
    let path = key(path);
    if let Some(asset) = memory_get(&path) {
        return Some(asset);
    }
    let mut bytes = vec![];
    fs::File::open(&path)
        .ok()?
        .take(MAX_FILE as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let asset = decode(path, bytes).ok()?;
    allowed().then(|| remember(asset))
}
pub fn cached_request(directory: &Path, request: Request) -> Option<Arc<Asset>> {
    if !request.valid() {
        return None;
    }
    request
        .candidates()
        .into_iter()
        .find_map(|r| cached(&directory.join(r.filename())))
}
/// Calls may run concurrently on workers. Publication coalesces decoded identity; failures and
/// fallback lookups are never inserted under the requested variant's key.
pub fn load_with(
    directory: &Path,
    request: Request,
    fetch: impl FnMut(&str) -> Result<Vec<u8>, String>,
) -> Result<Arc<Asset>, String> {
    load_cancellable_with(
        directory,
        request,
        &std::sync::atomic::AtomicBool::new(false),
        fetch,
    )
}

pub fn load_cancellable_with(
    directory: &Path,
    request: Request,
    cancel: &std::sync::atomic::AtomicBool,
    mut fetch: impl FnMut(&str) -> Result<Vec<u8>, String>,
) -> Result<Arc<Asset>, String> {
    if !request.valid() {
        return Err("Unsupported Pokémon sprite".into());
    }
    let mut last = "Pokémon image unavailable".to_string();
    for candidate in request.candidates() {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("Pokémon sprite request cancelled".into());
        }
        let path = directory.join(candidate.filename());
        if let Some(asset) =
            cached_checked(&path, || !cancel.load(std::sync::atomic::Ordering::Relaxed))
        {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err("Pokémon sprite request cancelled".into());
            }
            return Ok(asset);
        }
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("Pokémon sprite request cancelled".into());
        }
        match fetch(&candidate.url()).and_then(|bytes| {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err("Pokémon sprite request cancelled".into());
            }
            let mut asset = decode(key(&path), bytes)?;
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err("Pokémon sprite request cancelled".into());
            }
            super::api::atomic_write(&path, &asset.encoded)?;
            asset.path = key(&path);
            Ok(remember(asset))
        }) {
            Ok(asset) => return Ok(asset),
            Err(error) => last = error,
        }
    }
    Err(last)
}
#[cfg(test)]
#[path = "tests/sprites.rs"]
mod tests;
