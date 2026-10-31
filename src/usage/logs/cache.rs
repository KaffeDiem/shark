use super::{Entry, Provider, Roots, claude, codex, dedup_claude};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
#[derive(Clone, Debug, Default, PartialEq, Eq)]
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
#[derive(Clone, Debug, Default)]
struct Blob {
    stamp: Stamp,
    offset: Option<u64>,
    append_guard: Option<(u64, u64)>,
    entries: Vec<Entry>,
    ledger: BTreeMap<String, f64>,
    rollout: Option<codex::Rollout>,
}
#[derive(Clone, Debug)]
struct Probe {
    stamp: Stamp,
    session: Option<String>,
}
#[derive(Clone, Debug, Default)]
struct Snapshot {
    blobs: BTreeMap<PathBuf, Blob>,
    index: BTreeMap<PathBuf, Probe>,
}
#[derive(Default)]
pub struct Cache {
    snapshot: Snapshot,
    pub parsed_files: usize,
    pub parsed_bytes: u64,
    pub probes: usize,
    pub last_error: Option<String>,
}
impl Cache {
    #[cfg(test)]
    pub fn blob_count(&self) -> usize {
        self.snapshot.blobs.len()
    }
    #[cfg(test)]
    pub fn index_count(&self) -> usize {
        self.snapshot.index.len()
    }
    #[cfg(test)]
    pub fn entries(&mut self, provider: Provider, roots: &Roots) -> Vec<Entry> {
        self.entries_since(provider, roots, f64::NEG_INFINITY)
    }
    pub fn entries_since(&mut self, provider: Provider, roots: &Roots, since: f64) -> Vec<Entry> {
        self.entries_with_probe(provider, roots, since, |p| codex::probe(p, 1 << 20))
    }
    pub fn entries_with_probe(
        &mut self,
        provider: Provider,
        roots: &Roots,
        since: f64,
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
            // A file that vanished since the walk is gone, not a failed scan.
            .filter_map(|p| Stamp::at(&p).ok().map(|s| (p, s)))
            .collect();
        if self.last_error.is_none() {
            self.snapshot.blobs.retain(|p, _| stamps.contains_key(p));
            self.snapshot.index.retain(|p, _| stamps.contains_key(p));
        }
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
                if error.kind() != io::ErrorKind::NotFound {
                    self.last_error = Some(format!("Local usage read failed: {error}"));
                }
                return;
            }
        };
        // A larger replacement is not an append. Identity and bounded edge fingerprints
        // protect rotations and in-place rewrites without keeping transcript text.
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

/// Roots are the user's choice, so a symlinked root is followed; below them symlinks are
/// skipped and unreadable or vanished entries are ignored rather than aborting the scan.
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
    if fs::metadata(&canonical)?.is_file() {
        if canonical.extension().is_some_and(|e| e == "jsonl") {
            files.insert(canonical);
        }
        return Ok(());
    }
    visit(&canonical, files)
}

fn visit(dir: &Path, files: &mut BTreeSet<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => {
                let _ = visit(&path, files);
            }
            Ok(kind) if kind.is_file() && path.extension().is_some_and(|e| e == "jsonl") => {
                files.insert(path);
            }
            _ => {}
        }
    }
    Ok(())
}
