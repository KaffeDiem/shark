//! Runtime-only PokéAPI metadata and sprite cache. No Pokémon assets ship in Shark.
//! These blocking calls must run on the background executor, never during render.
use super::{EvoLine, EvoNode, Rarity, next_random, seed};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_BYTES: usize = 8 * 1024 * 1024;
const TTL: u64 = 30 * 86400;
#[derive(Clone, Debug)]
pub struct PokeApi {
    cache: PathBuf,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BaseSpecies {
    pub id: u16,
    pub capture_rate: u16,
}
#[derive(Serialize, Deserialize)]
struct CachedJson {
    fetched_at: u64,
    value: Value,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn sprite_url(id: u16, shiny: bool) -> String {
    super::forms::sprite_url(id, true, shiny, None)
}

impl PokeApi {
    pub fn new(cache: PathBuf) -> Self {
        Self { cache }
    }
    fn json(&self, key: &str, url: &str, body: Option<Value>) -> Result<Value, String> {
        self.json_with_fetch(key, || {
            fetch(url, body.as_ref().map(Value::to_string).as_deref())
        })
    }
    fn json_with_fetch(
        &self,
        key: &str,
        fetch_data: impl FnOnce() -> Result<Vec<u8>, String>,
    ) -> Result<Value, String> {
        let path = self.cache.join(format!("{key}.json"));
        let disk = read_bounded(&path)
            .ok()
            .and_then(|d| serde_json::from_slice::<CachedJson>(&d).ok())
            .filter(|cached| cached.fetched_at <= now() && valid_payload(key, &cached.value));
        if let Some(ref cached) = disk {
            if now().saturating_sub(cached.fetched_at) < TTL {
                return Ok(cached.value.clone());
            }
        }
        let fetched = fetch_data()
            .and_then(|bytes| {
                serde_json::from_slice::<Value>(&bytes)
                    .map_err(|_| "PokéAPI returned invalid JSON".into())
            })
            .and_then(|value| {
                if valid_payload(key, &value) {
                    Ok(value)
                } else {
                    Err("PokéAPI returned incomplete data".into())
                }
            });
        match fetched {
            Ok(value) => {
                let cached = CachedJson {
                    fetched_at: now(),
                    value: value.clone(),
                };
                if let Ok(bytes) = serde_json::to_vec(&cached) {
                    let _ = atomic_write(&path, &bytes);
                }
                Ok(value)
            }
            Err(error) => disk.map(|d| d.value).ok_or(error),
        }
    }
    fn species(&self, id: u16) -> Result<Value, String> {
        if !(1..=649).contains(&id) {
            return Err("Unsupported Pokémon species".into());
        }
        self.json(
            &format!("species-{id}"),
            &format!("https://pokeapi.co/api/v2/pokemon-species/{id}/"),
            None,
        )
    }
    pub fn resource_names(
        &self,
        resource: &super::names_client::Resource,
    ) -> Result<super::names_client::Names, String> {
        super::names_client::Client::shared(self.cache.join("names-v1")).names(resource)
    }
    pub fn details(&self, species_id: u16) -> Result<super::profile::Details, String> {
        let species = self.species(species_id)?;
        let pokemon = self.json(
            &format!("pokemon-{species_id}"),
            &format!("https://pokeapi.co/api/v2/pokemon/{species_id}/"),
            None,
        )?;
        parse_details(species_id, &species, &pokemon)
    }
    pub fn base_species_index(&self) -> Result<Vec<BaseSpecies>, String> {
        let value=self.json("base-index","https://graphql.pokeapi.co/v1beta2",Some(json!({"query":"{ pokemonspecies(where: {evolves_from_species_id: {_is_null: true}, id: {_lte: 649, _neq: 132}}, order_by: {id: asc}) { id capture_rate } }"})))?;
        let rows = value
            .pointer("/data/pokemonspecies")
            .and_then(Value::as_array)
            .ok_or("PokéAPI base index is unavailable")?;
        let mut seen = BTreeSet::new();
        let entries = rows
            .iter()
            .filter_map(|row| {
                let id = u16::try_from(row.get("id")?.as_u64()?).ok()?;
                let capture_rate = u16::try_from(row.get("capture_rate")?.as_u64()?).ok()?;
                ((1..=649).contains(&id)
                    && id != 132
                    && (1..=255).contains(&capture_rate)
                    && seen.insert(id))
                .then_some(BaseSpecies { id, capture_rate })
            })
            .collect::<Vec<_>>();
        if entries.is_empty() {
            Err("PokéAPI returned no hatch candidates".into())
        } else {
            Ok(entries)
        }
    }
    pub fn line(&self, base_id: u16) -> Result<EvoLine, String> {
        let base = self.species(base_id)?;
        let chain_url = base
            .pointer("/evolution_chain/url")
            .and_then(Value::as_str)
            .ok_or("Missing evolution chain")?;
        let chain_id = validated_chain_id(chain_url).ok_or("Invalid evolution chain URL")?;
        let chain = self.json(
            &format!("chain-{chain_id}"),
            &format!("https://pokeapi.co/api/v2/evolution-chain/{chain_id}/"),
            None,
        )?;
        let tree = parse_node(chain.get("chain").ok_or("Missing evolution tree")?, 0)?
            .keeping_animated_sprites()
            .ok_or("No animated Pokémon in evolution tree")?;
        if tree.species_id != base_id {
            return Err("Pokémon is not the start of its evolution line".into());
        }
        let rate = base
            .get("capture_rate")
            .and_then(Value::as_u64)
            .and_then(|v| u16::try_from(v).ok())
            .filter(|v| *v <= 255)
            .ok_or("Invalid capture rate")?;
        let rarity = Rarity::from_capture_rate(
            rate,
            base.get("is_legendary")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            base.get("is_mythical")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        );
        let mut names = BTreeMap::new();
        let mut ids = vec![];
        all_ids(&tree, &mut ids);
        for id in ids {
            let species = if id == base_id {
                base.clone()
            } else {
                self.species(id)?
            };
            let mut languages = BTreeMap::new();
            if let Some(rows) = species.get("names").and_then(Value::as_array) {
                for row in rows {
                    if let (Some(language), Some(name)) = (
                        row.pointer("/language/name").and_then(Value::as_str),
                        row.get("name").and_then(Value::as_str),
                    ) {
                        if !name.trim().is_empty() {
                            languages.insert(language.to_owned(), name.to_owned());
                        }
                    }
                }
            }
            languages = super::evolution::normalized_names(&languages);
            if !languages.contains_key("en") {
                if let Some(name) = species.get("name").and_then(Value::as_str) {
                    languages.insert("en".into(), name.into());
                }
            }
            names.insert(id, languages);
        }
        Ok(EvoLine {
            base_id,
            tree,
            rarity,
            names,
        })
    }
    pub fn random_line(&self, min_rarity: Option<Rarity>) -> Result<EvoLine, String> {
        self.random_line_with_collection(min_rarity, &BTreeSet::new())
    }
    pub fn random_line_with_collection(
        &self,
        min_rarity: Option<Rarity>,
        collected: &BTreeSet<u16>,
    ) -> Result<EvoLine, String> {
        let mut rng = seed();
        let id = choose_base(
            self.base_species_index(),
            min_rarity,
            collected,
            &mut rng,
            |id| self.base_species(id),
        )?;
        self.line(id)
    }

    pub fn base_species(&self, id: u16) -> Result<Option<BaseSpecies>, String> {
        if id == 132 {
            return Ok(None);
        }
        let value = self.species(id)?;
        if !value
            .get("evolves_from_species")
            .is_some_and(Value::is_null)
        {
            return Ok(None);
        }
        let capture_rate = value
            .get("capture_rate")
            .and_then(Value::as_u64)
            .filter(|v| (1..=255).contains(v));
        Ok(capture_rate.map(|rate| BaseSpecies {
            id,
            capture_rate: rate as u16,
        }))
    }
    pub fn cached_sprite(&self, id: u16, shiny: bool) -> Result<PathBuf, String> {
        self.cached_sprite_form(id, shiny, None)
    }
    pub fn cached_sprite_form_cancellable(
        &self,
        id: u16,
        shiny: bool,
        form: Option<&super::forms::Form>,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<PathBuf, String> {
        super::sprites::load_cancellable_with(
            &self.cache,
            super::sprites::Request::pokemon(id, true, shiny, form),
            &cancel,
            |url| fetch(url, None),
        )
        .map(|asset| asset.path.clone())
    }
    pub fn cached_sprite_form(
        &self,
        id: u16,
        shiny: bool,
        form: Option<&super::forms::Form>,
    ) -> Result<PathBuf, String> {
        super::sprites::load_with(
            &self.cache,
            super::sprites::Request::pokemon(id, true, shiny, form),
            |url| fetch(url, None),
        )
        .map(|asset| asset.path.clone())
    }
}
pub fn weighted_pick(entries: &[BaseSpecies], collected: &BTreeSet<u16>, roll: u64) -> Option<u16> {
    let total = entries
        .iter()
        .map(|e| adjusted_weight(e.capture_rate as u64, collected.contains(&e.id)))
        .sum::<u64>();
    if total == 0 {
        return None;
    }
    let mut ticket = roll % total;
    for entry in entries {
        let weight = adjusted_weight(entry.capture_rate as u64, collected.contains(&entry.id));
        if ticket < weight {
            return Some(entry.id);
        }
        ticket -= weight;
    }
    None
}
pub fn adjusted_weight(weight: u64, collected: bool) -> u64 {
    if collected {
        (weight / 2).max(1)
    } else {
        weight.max(1)
    }
}
pub fn validated_chain_id(value: &str) -> Option<u32> {
    let url = url::Url::parse(value).ok()?;
    if url.scheme() != "https"
        || url.host_str() != Some("pokeapi.co")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let path = url.path().trim_end_matches('/');
    let id = path
        .strip_prefix("/api/v2/evolution-chain/")?
        .parse::<u32>()
        .ok()?;
    (id > 0).then_some(id)
}
pub fn parse_node(value: &Value, depth: usize) -> Result<EvoNode, String> {
    if depth > 8 {
        return Err("Evolution tree is too deep".into());
    }
    let species_url = value
        .pointer("/species/url")
        .and_then(Value::as_str)
        .ok_or("Missing species URL")?;
    let url = url::Url::parse(species_url).map_err(|_| "Invalid species URL")?;
    if url.scheme() != "https" || url.host_str() != Some("pokeapi.co") {
        return Err("Invalid species URL".into());
    }
    let species_id = url
        .path()
        .trim_end_matches('/')
        .strip_prefix("/api/v2/pokemon-species/")
        .and_then(|id| id.parse::<u16>().ok())
        .filter(|id| *id > 0)
        .ok_or("Invalid species ID")?;
    let children = value
        .get("evolves_to")
        .and_then(Value::as_array)
        .ok_or("Missing evolution children")?;
    if children.len() > 32 {
        return Err("Too many evolution branches".into());
    }
    Ok(EvoNode {
        species_id,
        children: children
            .iter()
            .map(|n| parse_node(n, depth + 1))
            .collect::<Result<_, _>>()?,
    })
}
fn all_ids(node: &EvoNode, out: &mut Vec<u16>) {
    out.push(node.species_id);
    for child in &node.children {
        all_ids(child, out);
    }
}
pub(super) fn valid_gif(bytes: &[u8]) -> bool {
    const MAX_DECODED_BYTES: usize = 32 * 1024 * 1024;
    if bytes.len() < 13 || bytes.len() > MAX_BYTES {
        return false;
    }
    let mut options = gif::DecodeOptions::new();
    options.set_memory_limit(gif::MemoryLimit::Bytes(
        (4 * 1024 * 1024).try_into().unwrap(),
    ));
    options.check_frame_consistency(true);
    // Some official PokéAPI GIFs finish their pixel stream without an explicit LZW end
    // code (including Pikachu, Bulbasaur and Charmander). The standard decoder accepts
    // these. Keep structural/frame consistency and allocation limits below.
    options.check_lzw_end_code(false);
    let Ok(mut decoder) = options.read_info(bytes) else {
        return false;
    };
    if !(1..=1024).contains(&decoder.width()) || !(1..=1024).contains(&decoder.height()) {
        return false;
    }
    // The renderer composites each frame to the logical canvas, even when the encoded
    // frame is tiny. Bound that eventual allocation, rather than only compressed bytes.
    let frame_bytes = decoder.width() as usize * decoder.height() as usize * 4;
    let mut frames = 0usize;
    loop {
        match decoder.read_next_frame() {
            Ok(Some(_)) => {
                frames += 1;
                if frames > 1024 || frames.saturating_mul(frame_bytes) > MAX_DECODED_BYTES {
                    return false;
                }
            }
            Ok(None) => return frames > 0,
            Err(_) => return false,
        }
    }
}
pub(super) fn fetch(url: &str, body: Option<&str>) -> Result<Vec<u8>, String> {
    let mut command = Command::new("/usr/bin/curl");
    command.args([
        "--disable",
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--connect-timeout",
        "5",
        "--max-time",
        "20",
        "--max-filesize",
        "8388608",
    ]);
    if let Some(body) = body {
        command.args([
            "--header",
            "Content-Type: application/json",
            "--data-binary",
            body,
        ]);
    }
    command.arg(url);
    fetch_command(command)
}

fn fetch_command(mut command: Command) -> Result<Vec<u8>, String> {
    // Bound the pipe ourselves: older macOS curl versions only enforce --max-filesize
    // when Content-Length is known, and Command::output buffers the entire response.
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "Could not start Pokémon data request")?;
    let mut bytes = Vec::new();
    let read = child
        .stdout
        .take()
        .expect("piped Pokémon response")
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes);
    if read.is_err() || bytes.len() > MAX_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        return Err(if bytes.len() > MAX_BYTES {
            "Pokémon response is too large"
        } else {
            "Could not read Pokémon data"
        }
        .into());
    }
    let status = match child.wait() {
        Ok(status) => status,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Could not finish Pokémon data request".into());
        }
    };
    if !status.success() {
        return Err("Could not download Pokémon data; try again when online".into());
    }
    Ok(bytes)
}

pub(super) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("Invalid cache path")?;
    fs::create_dir_all(parent).map_err(|_| "Could not create Pokémon cache")?;
    let temporary = parent.join(format!(".pokemon-{}-{}.tmp", std::process::id(), seed()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })()
    .map_err(|_: std::io::Error| "Could not save Pokémon cache".to_string());
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

/// Normalize only Black 2/White 2 metadata; other-generation move tables must not leak in.
pub fn parse_details(
    species_id: u16,
    species: &Value,
    pokemon: &Value,
) -> Result<super::profile::Details, String> {
    use super::profile::{Ability, Details, LearnMethod, Move};
    let stat_rows = pokemon
        .get("stats")
        .and_then(Value::as_array)
        .ok_or("Missing Pokémon stats")?;
    let mut base_stats = BTreeMap::new();
    for stat in stat_rows {
        if let (Some(name), Some(value)) = (
            stat.pointer("/stat/name").and_then(Value::as_str),
            stat.get("base_stat").and_then(Value::as_i64),
        ) {
            if super::profile::STAT_ORDER.contains(&name) && (1..=255).contains(&value) {
                base_stats.insert(name.to_owned(), value as i32);
            }
        }
    }
    if base_stats.len() != 6 {
        return Err("Incomplete Pokémon stats".into());
    }
    let mut abilities = vec![];
    for item in pokemon
        .get("abilities")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let (Some(name), Some(slot), Some(hidden)) = (
            item.pointer("/ability/name").and_then(Value::as_str),
            item.get("slot").and_then(Value::as_i64),
            item.get("is_hidden").and_then(Value::as_bool),
        ) {
            if (1..=8).contains(&slot) && name.len() <= 80 {
                abilities.push(Ability {
                    name: name.into(),
                    slot: slot as i32,
                    is_hidden: hidden,
                });
            }
        }
    }
    abilities.sort_by_key(|a| a.slot);
    let mut moves = vec![];
    for item in pokemon
        .get("moves")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(name) = item
            .pointer("/move/name")
            .and_then(Value::as_str)
            .filter(|n| n.len() <= 80)
        else {
            continue;
        };
        let mut methods = vec![];
        for row in item
            .get("version_group_details")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if row.pointer("/version_group/name").and_then(Value::as_str) != Some("black-2-white-2")
            {
                continue;
            }
            if let (Some(method), Some(level)) = (
                row.pointer("/move_learn_method/name")
                    .and_then(Value::as_str),
                row.get("level_learned_at").and_then(Value::as_i64),
            ) {
                if (0..=100).contains(&level) && method.len() <= 80 {
                    methods.push(LearnMethod {
                        method: method.into(),
                        level: level as i32,
                    });
                }
            }
        }
        if !methods.is_empty() {
            moves.push(Move {
                name: name.into(),
                learn_methods: methods,
            });
        }
    }
    moves.sort_by(|a, b| a.name.cmp(&b.name));
    let gender_rate = species
        .get("gender_rate")
        .and_then(Value::as_i64)
        .filter(|v| (-1..=8).contains(v))
        .unwrap_or(-1) as i32;
    let mut types = Vec::new();
    for name in pokemon
        .get("types")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| row.pointer("/type/name").and_then(Value::as_str))
    {
        if name.bytes().all(|c| c.is_ascii_lowercase())
            && super::types::Type::parse(name).is_some()
            && !types.iter().any(|existing| existing == name)
        {
            types.push(name.to_owned());
            if types.len() == 2 {
                break;
            }
        }
    }
    Ok(Details {
        species_id,
        types,
        gender_rate,
        base_stats,
        abilities,
        moves,
    })
}

/// One production sampler with injectable data lookup for deterministic offline regressions.
/// GraphQL index failure switches to independent REST rejection sampling.
pub fn choose_base(
    index: Result<Vec<BaseSpecies>, String>,
    min_rarity: Option<Rarity>,
    collected: &BTreeSet<u16>,
    rng: &mut u64,
    mut species: impl FnMut(u16) -> Result<Option<BaseSpecies>, String>,
) -> Result<u16, String> {
    if min_rarity == Some(Rarity::Legendary) {
        return Err("Legendary-only eggs are not supported".into());
    }
    if let Ok(entries) = index {
        let pool = entries
            .into_iter()
            .filter(|e| {
                (1..=649).contains(&e.id)
                    && e.id != 132
                    && (1..=255).contains(&e.capture_rate)
                    && min_rarity.is_none_or(|r| r.includes(e.capture_rate))
            })
            .collect::<Vec<_>>();
        return weighted_pick(&pool, collected, next_random(rng))
            .ok_or_else(|| "No Pokémon match this egg grade".into());
    }
    for _ in 0..32 {
        let id = (next_random(rng) % 649 + 1) as u16;
        if id == 132 {
            continue;
        }
        let Some(candidate) = species(id)? else {
            continue;
        };
        if candidate.id != id
            || candidate.capture_rate == 0
            || candidate.capture_rate > 255
            || min_rarity.is_some_and(|r| !r.includes(candidate.capture_rate))
        {
            continue;
        }
        let weight = adjusted_weight(candidate.capture_rate as u64, collected.contains(&id));
        if next_random(rng) % 255 < weight {
            return Ok(id);
        }
    }
    Err("Pokémon data is temporarily unavailable. Your egg progress is saved.".into())
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, std::io::Error> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Pokémon cache exceeds size limit",
        ));
    }
    Ok(bytes)
}
fn valid_payload(key: &str, value: &Value) -> bool {
    if !value.is_object() || value.get("errors").is_some() || value.get("detail").is_some() {
        return false;
    }
    if key == "base-index" {
        return value
            .pointer("/data/pokemonspecies")
            .and_then(Value::as_array)
            .is_some_and(|v| !v.is_empty());
    }
    if let Some(id) = key
        .strip_prefix("species-")
        .and_then(|id| id.parse::<u64>().ok())
    {
        return value.get("id").and_then(Value::as_u64) == Some(id)
            && value.get("names").is_some_and(Value::is_array)
            && value
                .get("capture_rate")
                .and_then(Value::as_u64)
                .is_some_and(|n| (1..=255).contains(&n))
            && value
                .pointer("/evolution_chain/url")
                .and_then(Value::as_str)
                .and_then(validated_chain_id)
                .is_some();
    }
    if let Some(id) = key
        .strip_prefix("chain-")
        .and_then(|id| id.parse::<u64>().ok())
    {
        return value.get("id").and_then(Value::as_u64) == Some(id)
            && value
                .get("chain")
                .is_some_and(|tree| parse_node(tree, 0).is_ok());
    }
    if let Some(id) = key
        .strip_prefix("pokemon-")
        .and_then(|id| id.parse::<u16>().ok())
    {
        return value.get("id").and_then(Value::as_u64) == Some(id as u64)
            && parse_details(id, &Value::Null, value).is_ok();
    }
    true
}
#[cfg(test)]
mod cache_tests {
    use super::*;
    fn minimal_gif() -> Vec<u8> {
        animation(1, 1, 1)
    }
    fn animation(width: u16, height: u16, frames: usize) -> Vec<u8> {
        let mut bytes = vec![];
        {
            let mut encoder =
                gif::Encoder::new(&mut bytes, width, height, &[0, 0, 0, 255, 255, 255]).unwrap();
            for _ in 0..frames {
                let frame = gif::Frame {
                    width: 1,
                    height: 1,
                    buffer: std::borrow::Cow::Owned(vec![0]),
                    ..Default::default()
                };
                encoder.write_frame(&frame).unwrap();
            }
        }
        bytes
    }
    fn species(id: u64) -> Value {
        json!({"id":id,"names":[],"capture_rate":45,"evolution_chain":{"url":"https://pokeapi.co/api/v2/evolution-chain/1/"}})
    }
    fn temporary_cache() -> PathBuf {
        std::env::temp_dir().join(format!("shark-api-qa-{}-{}", std::process::id(), seed()))
    }
    fn store(cache: &Path, key: &str, fetched_at: u64, value: Value) {
        atomic_write(
            &cache.join(format!("{key}.json")),
            &serde_json::to_vec(&CachedJson { fetched_at, value }).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn corrupt_sprite_headers_and_compressed_animation_bombs_are_rejected() {
        assert!(valid_gif(&minimal_gif()));
        assert!(!valid_gif(b"GIF89a\x01\x00\x01\x00\x00\x00\x00"));
        let bytes = minimal_gif();
        assert!(!valid_gif(&bytes[..bytes.len() - 3]));
        assert!(valid_gif(&animation(1024, 1024, 8)));
        assert!(!valid_gif(&animation(1024, 1024, 9)));
        assert!(!valid_gif(&animation(1, 1, 1025)));
    }

    #[test]
    fn complete_pixel_stream_without_optional_lzw_end_code_is_accepted() {
        // One complete 1x1 pixel: clear code + pixel, without an explicit end code.
        // This reproduces the encoding found in real PokéAPI sprites without bundling art.
        let bytes = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x01\x44\x00;";
        let mut strict = gif::DecodeOptions::new();
        strict.check_lzw_end_code(true);
        let mut decoder = strict.read_info(&bytes[..]).unwrap();
        let rejected = loop {
            match decoder.read_next_frame() {
                Ok(Some(_)) => {}
                Ok(None) => break false,
                Err(_) => break true,
            }
        };
        assert!(
            rejected,
            "strict checking rejects the missing end code after flushing the complete pixel"
        );
        assert!(valid_gif(bytes));
        let decoded =
            super::super::sprites::decode(PathBuf::from("pixel.gif"), bytes.to_vec()).unwrap();
        assert_eq!(decoded.dimensions(), (1, 1));
        assert_eq!(decoded.frames.len(), 1);
        assert!(!valid_gif(&bytes[..bytes.len() - 3]));
    }

    #[test]
    fn metadata_cache_checks_resource_identity_and_complete_stats() {
        assert!(valid_payload("species-1", &species(1)));
        assert!(!valid_payload("species-1", &species(25)));
        assert!(!valid_payload(
            "species-1",
            &json!({"id":1,"names":[],"capture_rate":45})
        ));
        assert!(!valid_payload(
            "pokemon-1",
            &json!({"id":1,"stats":[{}, {}, {}, {}, {}, {}]})
        ));
        let tree = json!({"species":{"url":"https://pokeapi.co/api/v2/pokemon-species/1/"},"evolves_to":[]});
        assert!(valid_payload("chain-1", &json!({"id":1,"chain":tree})));
        assert!(!valid_payload("chain-1", &json!({"id":2,"chain":tree})));
        assert!(!valid_payload("chain-1", &json!({"id":1,"chain":{}})));
    }

    #[test]
    fn cache_hit_skips_network_stale_data_survives_offline_and_corruption_recovers() {
        let cache = temporary_cache();
        let api = PokeApi::new(cache.clone());
        store(&cache, "species-1", now(), species(1));
        assert_eq!(
            api.json_with_fetch("species-1", || panic!("fresh cache must not fetch"))
                .unwrap(),
            species(1)
        );
        store(&cache, "species-1", now() - TTL - 1, species(1));
        assert_eq!(
            api.json_with_fetch("species-1", || Err("offline".into()))
                .unwrap(),
            species(1)
        );
        store(&cache, "species-1", now(), species(25));
        assert_eq!(
            api.json_with_fetch("species-1", || Ok(serde_json::to_vec(&species(1)).unwrap()))
                .unwrap(),
            species(1)
        );
        store(&cache, "species-1", u64::MAX, species(1));
        assert!(
            api.json_with_fetch("species-1", || Err("offline".into()))
                .is_err()
        );
        fs::remove_dir_all(cache).unwrap();
    }

    #[test]
    fn oversized_response_pipe_is_bounded_and_producer_is_reaped() {
        let start = std::time::Instant::now();
        let error = fetch_command(Command::new("/usr/bin/yes")).unwrap_err();
        assert_eq!(error, "Pokémon response is too large");
        assert!(
            start.elapsed().as_secs() < 5,
            "infinite producer must be killed promptly"
        );
    }

    #[test]
    fn oversized_cache_is_rejected_without_reading_unbounded_data() {
        let cache = temporary_cache();
        fs::create_dir_all(&cache).unwrap();
        let path = cache.join("large.json");
        fs::File::create(&path)
            .unwrap()
            .set_len(MAX_BYTES as u64 + 1)
            .unwrap();
        assert_eq!(
            read_bounded(&path).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        fs::remove_dir_all(cache).unwrap();
    }

    #[test]
    fn failed_graphql_payload_is_never_cached() {
        assert!(!valid_payload(
            "base-index",
            &json!({"errors":[{"message":"temporary"}]})
        ));
        assert!(!valid_payload(
            "base-index",
            &json!({"data":{"pokemonspecies":[]}})
        ));
        assert!(valid_payload(
            "base-index",
            &json!({"data":{"pokemonspecies":[{"id":1,"capture_rate":45}]}})
        ));
    }
    #[test]
    fn malformed_sprite_dimensions_are_rejected() {
        let mut bytes = minimal_gif();
        assert!(valid_gif(&bytes));
        bytes[6] = 255;
        bytes[7] = 255;
        assert!(!valid_gif(&bytes));
        assert!(!valid_gif(b"invalid"));
    }
}
