use super::{Entry, Provider, Roots, claude, codex, dedup_claude};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
const VERSION: u32 = 2;
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct Stamp {
    size: u64,
    modified: u128,
    device: u64,
    inode: u64,
}
impl Stamp {
    fn at(path: &Path) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let m = fs::metadata(path)?;
        Ok(Self {
            size: m.len(),
            device: m.dev(),
            inode: m.ino(),
            modified: m
                .modified()?
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        })
    }
    fn seconds(&self) -> f64 {
        self.modified as f64 / 1e9
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Blob {
    stamp: Stamp,
    offset: Option<u64>,
    append_guard: Option<(u64, u64)>,
    entries: Vec<Entry>,
    ledger: BTreeMap<String, f64>,
    rollout: Option<codex::Rollout>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Probe {
    stamp: Stamp,
    session: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Snapshot {
    version: u32,
    index_version: u32,
    blobs: BTreeMap<PathBuf, Blob>,
    index: BTreeMap<PathBuf, Probe>,
}
#[derive(Default)]
pub struct Cache {
    snapshot: Snapshot,
    path: Option<PathBuf>,
    last_save: Option<f64>,
    dirty: bool,
    pub parsed_files: usize,
    pub parsed_bytes: u64,
    pub probes: usize,
    pub last_error: Option<String>,
}
impl Cache {
    /// Default is memory-only. Disk I/O requires an explicitly selected Shark-owned path.
    pub fn at(path: PathBuf) -> Self {
        let mut out = Self {
            path: Some(path.clone()),
            ..Self::default()
        };
        if let Ok(bytes) = read_bounded(&path, 32 * 1024 * 1024) {
            let plain = inflate_limited(&bytes, 128 * 1024 * 1024).ok();
            let data = plain.as_deref().unwrap_or(&bytes);
            if let Ok(snapshot) = serde_json::from_slice(data) {
                out.snapshot = snapshot
            }
        }
        if out.snapshot.version != VERSION {
            out.snapshot.blobs.clear()
        }
        if out.snapshot.index_version != VERSION {
            out.snapshot.index.clear()
        }
        out.snapshot.version = VERSION;
        out.snapshot.index_version = VERSION;
        out
    }
    pub fn persists_to_disk(&self) -> bool {
        self.path.is_some()
    }
    pub fn blob_count(&self) -> usize {
        self.snapshot.blobs.len()
    }
    pub fn index_count(&self) -> usize {
        self.snapshot.index.len()
    }
    pub fn entries(&mut self, provider: Provider, roots: &Roots) -> Vec<Entry> {
        self.entries_since(provider, roots, f64::NEG_INFINITY, crate::usage::now())
    }
    pub fn entries_since(
        &mut self,
        provider: Provider,
        roots: &Roots,
        since: f64,
        now: f64,
    ) -> Vec<Entry> {
        self.entries_with_probe(provider, roots, since, now, |p| codex::probe(p, 1 << 20))
    }
    pub fn entries_with_probe(
        &mut self,
        provider: Provider,
        roots: &Roots,
        since: f64,
        now: f64,
        mut probe: impl FnMut(&Path) -> io::Result<Option<String>>,
    ) -> Vec<Entry> {
        self.last_error = None;
        let mut files = BTreeSet::new();
        for root in &roots.0 {
            if let Err(error) = walk(root, &mut files, &mut BTreeSet::new()) {
                self.last_error = Some(format!("Local usage scan failed: {error}"));
            }
        }

        let stamps: BTreeMap<_, _> = files
            .into_iter()
            .filter_map(|p| match Stamp::at(&p) {
                Ok(s) => Some((p, s)),
                Err(error) => {
                    self.last_error = Some(format!("Local usage scan failed: {error}"));
                    None
                }
            })
            .collect();
        let before = self.snapshot.blobs.len() + self.snapshot.index.len();
        if self.last_error.is_none() {
            self.snapshot.blobs.retain(|p, _| stamps.contains_key(p));
            self.snapshot.index.retain(|p, _| stamps.contains_key(p));
        }
        self.dirty |= before != self.snapshot.blobs.len() + self.snapshot.index.len();
        let included: BTreeSet<_> = stamps
            .iter()
            .filter(|(_, s)| s.seconds() >= since)
            .map(|(p, _)| p.clone())
            .collect();
        let mut needed = included.clone();
        if provider == Provider::Codex {
            for p in &included {
                self.load(provider, p, &stamps[p]);
                if let Some(r) = self
                    .snapshot
                    .blobs
                    .get(p)
                    .filter(|b| b.stamp == stamps[p])
                    .and_then(|b| b.rollout.as_ref())
                {
                    self.snapshot.index.insert(
                        p.clone(),
                        Probe {
                            stamp: stamps[p].clone(),
                            session: r.session.clone(),
                        },
                    );
                }
            }
            for (p, s) in &stamps {
                if self.snapshot.index.get(p).is_none_or(|old| old.stamp != *s) {
                    self.probes += 1;
                    match probe(p) {
                        Ok(session) => {
                            self.snapshot.index.insert(
                                p.clone(),
                                Probe {
                                    stamp: s.clone(),
                                    session,
                                },
                            );
                            self.dirty = true
                        }
                        Err(error) => {
                            self.last_error =
                                Some(format!("Local usage metadata scan failed: {error}"));
                            self.snapshot.index.remove(p);
                        }
                    }
                }
            }
            // Parent closure is independent of modification-time filtering; archived/old parents
            // establish replay identity but their own entries stay outside this query.
            loop {
                let before = needed.len();
                for p in needed.clone() {
                    if let Some(stamp) = stamps.get(&p) {
                        self.load(provider, &p, stamp);
                    }
                    if let Some(parent) = self
                        .snapshot
                        .blobs
                        .get(&p)
                        .and_then(|b| b.rollout.as_ref())
                        .and_then(|r| r.parent.as_ref())
                    {
                        for (p, index) in &self.snapshot.index {
                            if index.session.as_ref() == Some(parent) {
                                needed.insert(p.clone());
                            }
                        }
                    }
                }
                if needed.len() == before {
                    break;
                }
            }
        }
        for p in &needed {
            if let Some(stamp) = stamps.get(p) {
                self.load(provider, p, stamp);
            }
        }
        let result = if provider == Provider::Claude {
            dedup_claude(
                included
                    .iter()
                    .filter_map(|p| self.snapshot.blobs.get(p))
                    .flat_map(|b| {
                        let mut e = b.entries.clone();
                        claude::apply_ledger(&mut e, &b.ledger);
                        e
                    })
                    .collect(),
            )
        } else {
            let rollouts = needed
                .iter()
                .filter_map(|p| self.snapshot.blobs.get(p).and_then(|b| b.rollout.clone()))
                .collect();
            codex::resolve_included(
                rollouts,
                &included
                    .iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect(),
            )
        };
        self.save(now);
        result
    }
    fn load(&mut self, provider: Provider, path: &Path, stamp: &Stamp) {
        if self
            .snapshot
            .blobs
            .get(path)
            .is_some_and(|b| b.stamp == *stamp)
        {
            return;
        }
        let old = self.snapshot.blobs.get(path);
        let mut file = match fs::File::open(path) {
            Ok(file) => file,
            Err(error) => {
                self.last_error = Some(format!("Local usage read failed: {error}"));
                return;
            }
        };
        // A larger replacement is not an append. Identity and bounded edge fingerprints
        // protect rotations and in-place rewrites without persisting transcript text.
        let offset = if provider == Provider::Claude {
            old.filter(|b| {
                stamp.size > b.stamp.size
                    && stamp.device == b.stamp.device
                    && stamp.inode == b.stamp.inode
            })
            .filter(|b| {
                b.append_guard.is_some()
                    && append_guard(&mut file, b.stamp.size).ok() == b.append_guard
            })
            .and_then(|b| b.offset)
            .unwrap_or(0)
        } else {
            0
        };
        let next_guard = if provider == Provider::Claude {
            match append_guard(&mut file, stamp.size) {
                Ok(guard) => Some(guard),
                Err(error) => {
                    self.last_error = Some(format!("Local usage read failed: {error}"));
                    return;
                }
            }
        } else {
            None
        };
        if let Err(error) = file.seek(SeekFrom::Start(offset)) {
            self.last_error = Some(format!("Local usage read failed: {error}"));
            return;
        }
        if provider == Provider::Codex {
            match codex::parse_reader(std::io::BufReader::with_capacity(64 * 1024, file), path) {
                Ok(rollout) => {
                    self.parsed_files += 1;
                    self.parsed_bytes = self.parsed_bytes.saturating_add(stamp.size);
                    self.snapshot.blobs.insert(
                        path.to_owned(),
                        Blob {
                            stamp: stamp.clone(),
                            rollout: Some(rollout),
                            ..Blob::default()
                        },
                    );
                    self.dirty = true;
                }
                Err(error) => self.last_error = Some(format!("Local usage read failed: {error}")),
            }
            return;
        }
        let parsed = match claude::parse_reader(
            std::io::BufReader::with_capacity(64 * 1024, file),
            path,
            offset,
        ) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.last_error = Some(format!("Local usage read failed: {error}"));
                return;
            }
        };
        self.parsed_files += 1;
        self.parsed_bytes = self
            .parsed_bytes
            .saturating_add(stamp.size.saturating_sub(offset));
        let blob = {
            let mut entries = if offset > 0 {
                old.map(|b| b.entries.clone()).unwrap_or_default()
            } else {
                vec![]
            };
            entries.extend(parsed.entries);
            let ledger = if parsed.ledger.is_empty() && offset > 0 {
                old.map(|b| b.ledger.clone()).unwrap_or_default()
            } else {
                parsed.ledger
            };
            Blob {
                stamp: stamp.clone(),
                offset: Some(parsed.offset),
                append_guard: next_guard,
                entries: dedup_claude(entries),
                ledger,
                rollout: None,
            }
        };
        self.snapshot.blobs.insert(path.to_owned(), blob);
        self.dirty = true;
    }
    pub fn save(&mut self, now: f64) {
        let Some(path) = &self.path else { return };
        if !self.dirty || self.last_save.is_some_and(|t| now - t < 60.) {
            return;
        }
        let mut snapshot = self.snapshot.clone();
        snapshot.version = VERSION;
        snapshot.index_version = VERSION;
        snapshot
            .blobs
            .retain(|_, b| now - b.stamp.seconds() <= 40. * 86400.);
        let Ok(plain) = serde_json::to_vec(&snapshot) else {
            return;
        };
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        if encoder.write_all(&plain).is_err() {
            return;
        }
        let Ok(bytes) = encoder.finish() else { return };
        let temporary = path.with_extension("tmp");
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if fs::write(&temporary, bytes)
            .and_then(|_| fs::rename(&temporary, path))
            .is_ok()
        {
            self.dirty = false;
            self.last_save = Some(now)
        }
    }
}
fn append_guard(file: &mut fs::File, length: u64) -> io::Result<(u64, u64)> {
    use std::hash::{DefaultHasher, Hasher};
    let mut bytes = [0u8; 4096];
    let count = length.min(bytes.len() as u64) as usize;
    let mut hash_at = |offset| -> io::Result<u64> {
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut bytes[..count])?;
        let mut hasher = DefaultHasher::new();
        hasher.write(&bytes[..count]);
        Ok(hasher.finish())
    };
    Ok((hash_at(0)?, hash_at(length.saturating_sub(count as u64))?))
}

fn walk(
    path: &Path,
    files: &mut BTreeSet<PathBuf>,
    seen: &mut BTreeSet<PathBuf>,
) -> io::Result<()> {
    let canonical = match path.canonicalize() {
        Ok(p) => p,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if !seen.insert(canonical.clone()) {
        return Ok(());
    }
    let meta = fs::metadata(&canonical)?;
    if meta.is_file() {
        if canonical.extension().is_some_and(|e| e == "jsonl") {
            files.insert(canonical);
        }
        return Ok(());
    }
    for entry in fs::read_dir(canonical)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        walk(&entry.path(), files, seen)?;
    }
    Ok(())
}

fn read_bounded(path: &Path, max: u64) -> io::Result<Vec<u8>> {
    let f = fs::File::open(path)?;
    if f.metadata()?.len() > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "cache too large",
        ));
    }
    let mut b = vec![];
    f.take(max + 1).read_to_end(&mut b)?;
    if b.len() as u64 > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "cache too large",
        ));
    }
    Ok(b)
}

pub(super) fn inflate_limited(bytes: &[u8], limit: u64) -> io::Result<Vec<u8>> {
    let mut plain = vec![];
    flate2::read::ZlibDecoder::new(bytes)
        .take(limit.saturating_add(1))
        .read_to_end(&mut plain)?;
    if plain.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "inflated cache too large",
        ));
    }
    Ok(plain)
}
