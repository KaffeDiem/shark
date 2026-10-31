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
    BackupFailed(String),
}

impl Error {
    /// Keep diagnostic detail in the error value; show stable, translated instructions to users.
    pub fn user_message(&self, language: super::format::Language) -> String {
        use super::format::Language;
        let index = match language {
            Language::Ko => 0,
            Language::En => 1,
            Language::Ja => 2,
            Language::Es => 3,
            Language::Fr => 4,
            Language::Pt => 5,
            Language::De => 6,
            Language::Ru => 7,
        };
        let messages = match self {
            Self::NotASave => [
                "올바른 Shark 포켓몬 저장 파일이 아닙니다.",
                "This is not a valid Shark Pokémon save",
                "有効な Shark ポケモンのセーブファイルではありません。",
                "Este no es un archivo válido de Pokémon de Shark.",
                "Ce fichier de sauvegarde Pokémon de Shark n’est pas valide.",
                "Este não é um arquivo válido de Pokémon do Shark.",
                "Dies ist kein gültiger Pokémon-Spielstand von Shark.",
                "Это недопустимое сохранение покемонов Shark.",
            ],
            Self::NewerSchema(_) => [
                "이 저장 파일을 가져오려면 Shark를 업데이트하세요.",
                "Update Shark before importing this newer save",
                "この新しいセーブを読み込むには Shark を更新してください。",
                "Actualiza Shark antes de importar este archivo más reciente.",
                "Mettez Shark à jour avant d’importer cette sauvegarde plus récente.",
                "Atualize o Shark antes de importar este arquivo mais recente.",
                "Aktualisiere Shark, bevor du diesen neueren Spielstand importierst.",
                "Обновите Shark перед импортом этого нового сохранения.",
            ],
            Self::TooLarge => [
                "저장 파일은 8 MiB 이하여야 합니다.",
                "Pokémon saves must be no larger than 8 MiB",
                "セーブファイルは 8 MiB 以下にしてください。",
                "El archivo debe tener como máximo 8 MiB.",
                "La sauvegarde ne doit pas dépasser 8 MiB.",
                "O arquivo deve ter no máximo 8 MiB.",
                "Der Spielstand darf höchstens 8 MiB groß sein.",
                "Сохранение не должно превышать 8 МиБ.",
            ],
            Self::Io(_) => [
                "파일을 읽거나 쓸 수 없습니다. 위치와 접근 권한을 확인하세요.",
                "Could not read or write the save. Check its location and file permissions",
                "ファイルを読み書きできません。場所とアクセス権を確認してください。",
                "No se pudo leer o escribir el archivo. Comprueba su ubicación y permisos.",
                "Impossible de lire ou d’écrire le fichier. Vérifiez son emplacement et ses autorisations.",
                "Não foi possível ler ou gravar o arquivo. Verifique o local e as permissões.",
                "Die Datei konnte nicht gelesen oder geschrieben werden. Prüfe Ort und Zugriffsrechte.",
                "Не удалось прочитать или записать файл. Проверьте его расположение и права доступа.",
            ],
            Self::BackupFailed(_) => [
                "백업에 실패하여 진행 상황을 바꾸지 않았습니다.",
                "Progress was not replaced because its backup could not be saved",
                "バックアップを保存できなかったため、進行状況は変更されませんでした。",
                "No se reemplazó el progreso porque no se pudo guardar su copia de seguridad.",
                "La progression n’a pas été remplacée car sa copie de secours n’a pas pu être enregistrée.",
                "O progresso não foi substituído porque não foi possível salvar sua cópia de segurança.",
                "Der Fortschritt wurde nicht ersetzt, weil die Sicherung nicht gespeichert werden konnte.",
                "Прогресс не заменён, поскольку не удалось сохранить резервную копию.",
            ],
        };
        messages[index].into()
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.user_message(super::format::Language::En))
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

/// Internal history also accepts the earlier raw Rust companion shape. External imports
/// still require the branded envelope, so unrelated JSON cannot replace a collection.
pub(crate) fn read_snapshot(path: &Path) -> Result<Envelope, Error> {
    let mut bytes = Vec::new();
    let file = fs::File::open(path)?;
    let modified = file
        .metadata()?
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|t| t.as_secs())
        .unwrap_or(0);
    file.take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    match decode(&bytes) {
        Ok(envelope) => Ok(envelope),
        Err(Error::NotASave) => {
            let value: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|_| Error::NotASave)?;
            let object = value.as_object().ok_or(Error::NotASave)?;
            if object.contains_key("format")
                || object.contains_key("schema")
                || !object.contains_key("active")
            {
                return Err(Error::NotASave);
            }
            let mut state: Companion =
                serde_json::from_value(value).map_err(|_| Error::NotASave)?;
            state.sanitize();
            Ok(Envelope {
                format: FORMAT.into(),
                schema: SCHEMA,
                app_version: "legacy-rust".into(),
                exported_at: modified,
                state,
            })
        }
        Err(error) => Err(error),
    }
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
    pub current_species_id: Option<u16>,
    pub current_is_shiny: bool,
    pub current_form: Option<super::forms::Form>,
}

fn summary(path: PathBuf, state: &Companion, exported_at: u64) -> Snapshot {
    Snapshot {
        path,
        exported_at,
        lifetime_tokens: state.lifetime_tokens,
        collection_count: state.collection.len(),
        current_species_id: state.active.as_ref().map(super::Mon::species_id),
        current_is_shiny: state
            .active
            .as_ref()
            .is_some_and(super::Mon::displays_shiny),
        current_form: state
            .active
            .as_ref()
            .and_then(|m| super::forms::resolved(m.species_id(), m.form.as_ref())),
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
                let envelope = read_snapshot(&entry.path()).ok()?;
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
                        for old in self
                            .list()
                            .into_iter()
                            .filter(|old| old.path != snapshot.path)
                            .skip(KEEP_SNAPSHOTS - 1)
                        {
                            let _ = fs::remove_file(old.path);
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
        if self
            .list()
            .first()
            .is_some_and(|s| now.saturating_sub(s.exported_at) < AUTO_INTERVAL)
        {
            return Ok(None);
        }
        self.create(state, now).map(Some)
    }

    pub fn latest(&self) -> Option<Companion> {
        self.list()
            .first()
            .and_then(|s| read_snapshot(&s.path).ok())
            .map(|e| e.state)
    }

    /// Decode before creating a backup: pruning must not remove a requested old snapshot first.
    /// Callers persist the returned state only after this method succeeds.
    pub fn prepare_restore(
        &self,
        path: &Path,
        current: &Companion,
        now: u64,
        usage: BTreeMap<String, UsageBaseline>,
    ) -> Result<Companion, Error> {
        let imported = read_snapshot(path)?.state;
        self.create(current, now)
            .map_err(|e| Error::BackupFailed(e.to_string()))?;
        Ok(rebase(imported, current, usage))
    }
}

#[cfg(test)]
#[path = "tests/storage.rs"]
mod tests;
