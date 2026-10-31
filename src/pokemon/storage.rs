//! Bounded save transfer and recoverable snapshots for the Rust companion.
//! Shark's envelope is versioned separately from Swift PokeTokenBar saves.
use super::{Companion, LimitBaseline, UsageBaseline};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
pub const FORMAT: &str = "shark.pokemon.save";
pub const SCHEMA: u32 = 1;
pub const KEEP_SNAPSHOTS: usize = 10;
pub const AUTO_INTERVAL: u64 = 12 * 3600;

/// UTC date keeps exported files recognizable without depending on a locale-specific formatter.
pub fn suggested_file_name(now: u64) -> String {
    let z = (now.min(253_402_300_799) / 86_400) as i64 + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!("Shark-Pokemon-Save-{year:04}-{month:02}-{day:02}.json")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Envelope {
    pub format: String,
    pub schema: u32,
    pub app_version: String,
    pub exported_at: u64,
    pub state: Companion,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    NotASave,
    NewerSchema(u32),
    TooLarge,
    Io(String),
}

/// Diagnostic detail stays in the error value; users see stable, fixed instructions.
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotASave => "This is not a valid Shark Pokémon save",
            Self::NewerSchema(_) => "Update Shark before importing this newer save",
            Self::TooLarge => "Pokémon saves must be no larger than 8 MiB",
            Self::Io(_) => {
                "Could not read or write the save. Check its location and file permissions"
            }
        })
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

pub fn encode(state: &Companion, now: u64) -> Result<Vec<u8>, Error> {
    let bytes = serde_json::to_vec_pretty(&Envelope {
        format: FORMAT.into(),
        schema: SCHEMA,
        app_version: env!("CARGO_PKG_VERSION").into(),
        exported_at: now,
        state: state.clone(),
    })
    .map_err(|_| Error::NotASave)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(Error::TooLarge);
    }
    Ok(bytes)
}

pub fn decode(bytes: &[u8]) -> Result<Envelope, Error> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(Error::TooLarge);
    }
    #[derive(Deserialize)]
    struct Header {
        format: String,
        schema: u32,
    }
    let header: Header = serde_json::from_slice(bytes).map_err(|_| Error::NotASave)?;
    if header.format != FORMAT || header.schema == 0 {
        return Err(Error::NotASave);
    }
    if header.schema > SCHEMA {
        return Err(Error::NewerSchema(header.schema));
    }
    let mut envelope: Envelope = serde_json::from_slice(bytes).map_err(|_| Error::NotASave)?;
    envelope.state.sanitize();
    Ok(envelope)
}

pub fn read(path: &Path) -> Result<Envelope, Error> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    decode(&bytes)
}

/// Preserve progress, use this device's usage checkpoints, and merge already claimed rewards.
/// Settings such as visibility/language live in Shark's State and are never imported here.
pub fn rebase(
    mut imported: Companion,
    current: &Companion,
    usage: BTreeMap<String, UsageBaseline>,
) -> Companion {
    imported.set_growth_difficulty(current.growth_difficulty);
    imported.shop_difficulty = current.shop_difficulty;
    imported.baselines = usage;
    for (key, local) in &current.limit_baselines {
        let saved = imported.limit_baselines.entry(key.clone()).or_default();
        *saved = merge_limit(saved, local);
    }
    imported.sanitize();
    imported
}

fn merge_limit(a: &LimitBaseline, b: &LimitBaseline) -> LimitBaseline {
    if a.epoch == b.epoch {
        return LimitBaseline {
            epoch: a.epoch.clone(),
            claimed: a.claimed || b.claimed,
        };
    }
    // Epochs are fixed-width timestamp strings supplied by the provider adapter.
    match (a.claimed, b.claimed) {
        (true, false) => a.clone(),
        (false, true) => b.clone(),
        _ => {
            if a.epoch > b.epoch {
                a.clone()
            } else {
                b.clone()
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub path: PathBuf,
    pub exported_at: u64,
    pub lifetime_tokens: u64,
    pub collection_count: usize,
}

fn summary(path: PathBuf, state: &Companion, exported_at: u64) -> Snapshot {
    Snapshot {
        path,
        exported_at,
        lifetime_tokens: state.lifetime_tokens,
        collection_count: state.collection.len(),
    }
}

#[derive(Clone)]
pub struct Snapshots {
    pub directory: PathBuf,
}

impl Snapshots {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    /// Snapshot files newest first, from the `companion-snapshot-{time:020}-{n:05}.json` name alone.
    /// Writers run this on every state save, so it must not read file contents.
    fn named(&self) -> Vec<(u64, PathBuf)> {
        let Ok(entries) = fs::read_dir(&self.directory) else {
            return vec![];
        };
        let mut files: Vec<_> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let time = name
                    .strip_prefix("companion-snapshot-")?
                    .strip_suffix(".json")?
                    .split('-')
                    .next()?
                    .parse()
                    .ok()?;
                Some((time, entry.path()))
            })
            .collect();
        files.sort_by(|a, b| b.cmp(a));
        files
    }

    pub fn list(&self) -> Vec<Snapshot> {
        let Ok(entries) = fs::read_dir(&self.directory) else {
            return vec![];
        };
        let mut snapshots: Vec<_> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !name.starts_with("companion-snapshot-") || !name.ends_with(".json") {
                    return None;
                }
                let envelope = read(&entry.path()).ok()?;
                Some(summary(entry.path(), &envelope.state, envelope.exported_at))
            })
            .collect();
        snapshots.sort_by(|a, b| {
            b.exported_at
                .cmp(&a.exported_at)
                .then_with(|| b.path.cmp(&a.path))
        });
        snapshots
    }

    pub fn create(&self, state: &Companion, now: u64) -> Result<Snapshot, Error> {
        let bytes = encode(state, now)?;
        fs::create_dir_all(&self.directory)?;
        // Create-new temporary file plus hard link publishes atomically without ever replacing
        // an earlier snapshot, including multiple imports in the same second.
        let temporary = self.directory.join(format!(
            ".snapshot-{}-{}.partial",
            std::process::id(),
            super::seed()
        ));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            for counter in 0..10_000 {
                let path = self
                    .directory
                    .join(format!("companion-snapshot-{now:020}-{counter:05}.json"));
                match fs::hard_link(&temporary, &path) {
                    Ok(()) => {
                        let snapshot = summary(path, state, now);
                        // This is also the mandatory pre-restore backup. Never prune it
                        // immediately when the system clock moved behind existing snapshots.
                        for (_, old) in self
                            .named()
                            .into_iter()
                            .filter(|(_, old)| *old != snapshot.path)
                            .skip(KEEP_SNAPSHOTS - 1)
                        {
                            let _ = fs::remove_file(old);
                        }
                        return Ok(snapshot);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(e) => return Err(Error::from(e)),
                }
            }
            Err(Error::Io(
                "Too many snapshots created at the same instant".into(),
            ))
        })();
        let _ = fs::remove_file(&temporary);
        result
    }

    pub fn maybe_create(&self, state: &Companion, now: u64) -> Result<Option<Snapshot>, Error> {
        // A snapshot stamped after `now` (clock moved back) must not block new ones.
        if self
            .named()
            .iter()
            .find(|(time, _)| *time <= now)
            .is_some_and(|(time, _)| now - time < AUTO_INTERVAL)
        {
            return Ok(None);
        }
        self.create(state, now).map(Some)
    }

    pub fn latest(&self) -> Option<Companion> {
        self.list()
            .first()
            .and_then(|s| read(&s.path).ok())
            .map(|e| e.state)
    }
}

#[cfg(test)]
#[path = "tests/storage.rs"]
mod tests;
