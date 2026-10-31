//! Typed, bounded, single-flight PokéAPI labels; stable resource keys never become translations.
use super::evolution::{normalized_names, resolve_name};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};
pub type Names = BTreeMap<String, String>;
pub const TTL: u64 = 30 * 86400;
const MAX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_REQUESTS: usize = 8;
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Move,
    Ability,
    Type,
    PokemonForm,
}
impl Kind {
    pub fn path(self) -> &'static str {
        match self {
            Self::Move => "move",
            Self::Ability => "ability",
            Self::Type => "type",
            Self::PokemonForm => "pokemon-form",
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct Resource {
    pub kind: Kind,
    pub name: String,
}
impl Resource {
    pub fn new(kind: Kind, name: impl Into<String>) -> Self {
        Self {
            kind,
            name: name.into(),
        }
    }
    pub fn valid(&self) -> bool {
        !self.name.is_empty()
            && self.name.len() <= 80
            && self
                .name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    }
    pub fn key(&self) -> String {
        format!("{}-{}", self.kind.path(), self.name)
    }
    pub fn url(&self) -> Option<String> {
        self.valid().then(|| {
            format!(
                "https://pokeapi.co/api/v2/{}/{}/",
                self.kind.path(),
                self.name
            )
        })
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Snapshot {
    fetched_at: u64,
    names: Names,
}
#[derive(Default)]
struct Flight {
    result: Mutex<Option<Result<Snapshot, String>>>,
    ready: Condvar,
}
#[derive(Default)]
struct State {
    memory: BTreeMap<Resource, Snapshot>,
    pending: BTreeMap<Resource, Arc<Flight>>,
}
struct Inner {
    directory: Option<PathBuf>,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    fetch: Arc<dyn Fn(&str) -> Result<Vec<u8>, String> + Send + Sync>,
    state: Mutex<State>,
    active: Mutex<usize>,
    available: Condvar,
}
#[derive(Clone)]
pub struct Client(Arc<Inner>);
impl Client {
    pub fn new(directory: Option<PathBuf>) -> Self {
        Self::with_transport(
            directory,
            || {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            },
            |url| super::api::fetch(url, None),
        )
    }
    pub fn with_transport(
        directory: Option<PathBuf>,
        clock: impl Fn() -> u64 + Send + Sync + 'static,
        fetch: impl Fn(&str) -> Result<Vec<u8>, String> + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::new(Inner {
            directory,
            clock: Arc::new(clock),
            fetch: Arc::new(fetch),
            state: Mutex::default(),
            active: Mutex::new(0),
            available: Condvar::new(),
        }))
    }
    pub fn shared(directory: PathBuf) -> Self {
        static CLIENTS: OnceLock<Mutex<BTreeMap<PathBuf, Client>>> = OnceLock::new();
        let mut clients = CLIENTS
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let result = clients
            .entry(directory.clone())
            .or_insert_with(|| Self::new(Some(directory)))
            .clone();
        while clients.len() > 8 {
            clients.pop_first();
        }
        result
    }
    fn read(&self, resource: &Resource) -> Option<Snapshot> {
        let path = self
            .0
            .directory
            .as_ref()?
            .join(format!("{}.json", resource.key()));
        let mut bytes = vec![];
        fs::File::open(path)
            .ok()?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > MAX_BYTES {
            return None;
        }
        let mut snapshot: Snapshot = serde_json::from_slice(&bytes).ok()?;
        if snapshot.fetched_at > (self.0.clock)() {
            return None;
        }
        snapshot.names = normalized_names(&snapshot.names);
        Some(snapshot)
    }
    pub fn names(&self, resource: &Resource) -> Result<Names, String> {
        let url = resource.url().ok_or("Invalid Pokémon name resource")?;
        let now = (self.0.clock)();
        let (previous, flight, leader) = {
            let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(flight) = state.pending.get(resource) {
                (None, flight.clone(), false)
            } else {
                let previous = state
                    .memory
                    .get(resource)
                    .cloned()
                    .or_else(|| self.read(resource));
                if let Some(snapshot) = previous
                    .as_ref()
                    .filter(|s| s.fetched_at <= now && now - s.fetched_at < TTL)
                {
                    state.memory.insert(resource.clone(), snapshot.clone());
                    return Ok(snapshot.names.clone());
                }
                let flight = Arc::new(Flight::default());
                state.pending.insert(resource.clone(), flight.clone());
                (previous, flight, true)
            }
        };
        if !leader {
            let mut result = flight.result.lock().unwrap_or_else(|e| e.into_inner());
            while result.is_none() {
                result = flight.ready.wait(result).unwrap_or_else(|e| e.into_inner());
            }
            return result.as_ref().unwrap().clone().map(|s| s.names);
        }
        let result = {
            let mut active = self.0.active.lock().unwrap_or_else(|e| e.into_inner());
            while *active >= MAX_REQUESTS {
                active = self
                    .0
                    .available
                    .wait(active)
                    .unwrap_or_else(|e| e.into_inner());
            }
            *active += 1;
            drop(active);
            struct Slot<'a>(&'a Inner);
            impl Drop for Slot<'_> {
                fn drop(&mut self) {
                    let mut active = self.0.active.lock().unwrap_or_else(|e| e.into_inner());
                    *active -= 1;
                    self.0.available.notify_one();
                }
            }
            let _slot = Slot(&self.0);
            (self.0.fetch)(&url).and_then(|bytes| {
                if bytes.len() as u64 > MAX_BYTES {
                    return Err("Pokémon names response is too large".into());
                }
                let value: serde_json::Value =
                    serde_json::from_slice(&bytes).map_err(|_| "Invalid Pokémon names response")?;
                parse(resource.kind, &value).map(|names| Snapshot {
                    fetched_at: (self.0.clock)(),
                    names,
                })
            })
        };
        let fetched = result.is_ok();
        let result = result.or_else(|error| previous.ok_or(error));
        if let Ok(snapshot) = &result {
            if fetched {
                if let Some(directory) = &self.0.directory {
                    if let Ok(bytes) = serde_json::to_vec(snapshot) {
                        let _ = super::api::atomic_write(
                            &directory.join(format!("{}.json", resource.key())),
                            &bytes,
                        );
                    }
                }
            }
        }
        {
            let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Ok(snapshot) = &result {
                state.memory.insert(resource.clone(), snapshot.clone());
            }
            // Bound both entry count and approximate text bytes; disk retains evicted labels.
            while state.memory.len() > 512
                || state
                    .memory
                    .values()
                    .map(|s| {
                        s.names
                            .iter()
                            .map(|(k, v)| k.len() + v.len() + 96)
                            .sum::<usize>()
                    })
                    .sum::<usize>()
                    > MAX_BYTES as usize
            {
                state.memory.pop_first();
            }
            *flight.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(result.clone());
            state.pending.remove(resource);
            flight.ready.notify_all();
        }
        result.map(|snapshot| snapshot.names)
    }
}
pub fn parse(kind: Kind, value: &serde_json::Value) -> Result<Names, String> {
    // pokemon-form's ordinary names contain the species name; use its bare form_names.
    if !value.get("names").is_some_and(serde_json::Value::is_array) {
        return Err("Missing Pokémon names".into());
    }
    let field = if kind == Kind::PokemonForm {
        "form_names"
    } else {
        "names"
    };
    let mut names = Names::new();
    for row in value
        .get(field)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        if let (Some(code), Some(name)) = (
            row.pointer("/language/name")
                .and_then(serde_json::Value::as_str),
            row.get("name").and_then(serde_json::Value::as_str),
        ) {
            names.insert(code.into(), name.into());
        }
    }
    Ok(normalized_names(&names))
}
pub fn identifier(raw: &str) -> String {
    raw.split('-')
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}
pub fn label(
    resource: &Resource,
    preferred: &[&str],
    snapshots: &BTreeMap<Resource, Names>,
    failed: &BTreeSet<Resource>,
) -> String {
    match snapshots.get(resource) {
        Some(names) => resolve_name(names, preferred).unwrap_or_else(|| identifier(&resource.name)),
        None if failed.contains(resource) => identifier(&resource.name),
        None => "…".into(),
    }
}
#[derive(Clone)]
pub struct Item {
    pub resource: Resource,
    pub suffix: String,
}
pub fn text(
    items: &[Item],
    preferred: &[&str],
    snapshots: &BTreeMap<Resource, Names>,
    failed: &BTreeSet<Resource>,
) -> String {
    items
        .iter()
        .map(|item| label(&item.resource, preferred, snapshots, failed) + &item.suffix)
        .collect::<Vec<_>>()
        .join(" · ")
}
#[cfg(test)]
#[path = "tests/names_client.rs"]
mod tests;
