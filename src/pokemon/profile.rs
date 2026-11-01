//! Deterministic individual Pokémon profiles, ported from PokeTokenBar (MIT).
use super::Rarity;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_TOKENS: u64 = 1_000_000_000_000_000;
pub const STAT_ORDER: [&str; 6] = [
    "hp",
    "attack",
    "defense",
    "special-attack",
    "special-defense",
    "speed",
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Details {
    pub species_id: u16,
    #[serde(default)]
    pub types: Vec<String>,
    pub gender_rate: i32,
    pub base_stats: BTreeMap<String, i32>,
    pub abilities: Vec<Ability>,
    pub moves: Vec<Move>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ability {
    pub name: String,
    pub slot: i32,
    pub is_hidden: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Move {
    pub name: String,
    pub learn_methods: Vec<LearnMethod>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LearnMethod {
    pub method: String,
    pub level: i32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KnownMove {
    pub name: String,
    pub learned_at_level: i32,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Gender {
    Male,
    Female,
    Genderless,
}

impl Details {
    pub fn level_up_moves(&self, through: i32) -> Vec<KnownMove> {
        let mut names = BTreeMap::<String, i32>::new();
        for item in &self.moves {
            if let Some(level) = item
                .learn_methods
                .iter()
                .filter(|m| m.method == "level-up" && m.level <= through)
                .map(|m| m.level)
                .min()
            {
                names
                    .entry(item.name.clone())
                    .and_modify(|old| *old = (*old).min(level))
                    .or_insert(level);
            }
        }
        let mut moves: Vec<_> = names
            .into_iter()
            .map(|(name, learned_at_level)| KnownMove {
                name,
                learned_at_level,
            })
            .collect();
        moves.sort_by(|a, b| {
            a.learned_at_level
                .cmp(&b.learned_at_level)
                .then_with(|| a.name.cmp(&b.name))
        });
        moves
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub instance_id: String,
    pub seed: u64,
    pub gender: Option<Gender>,
    pub ivs: [i32; 6],
    pub ability_slot: Option<i32>,
    pub ability_name: Option<String>,
    pub ability_is_hidden: bool,
    pub level: i32,
    pub growth_tokens: u64,
    pub moves: Vec<KnownMove>,
}
impl Profile {
    pub fn generate(seed: u64, instance_id: String) -> Self {
        let mut rng = ProfileRng::new(seed);
        Self {
            instance_id,
            seed,
            gender: None,
            ivs: std::array::from_fn(|_| (rng.next() % 32) as i32),
            ability_slot: None,
            ability_name: None,
            ability_is_hidden: false,
            level: 5,
            growth_tokens: 0,
            moves: vec![],
        }
    }
    pub fn enrich(&mut self, details: &Details) {
        let mut rng = ProfileRng::new(self.seed ^ 0xA11B_1E5D_9EED);
        if self.gender.is_none() {
            self.gender = Some(if details.gender_rate < 0 {
                Gender::Genderless
            } else if (rng.next() % 8) < details.gender_rate as u64 {
                Gender::Female
            } else {
                Gender::Male
            });
        }
        let mut normal: Vec<_> = details.abilities.iter().filter(|a| !a.is_hidden).collect();
        let mut hidden: Vec<_> = details.abilities.iter().filter(|a| a.is_hidden).collect();
        normal.sort_by_key(|a| a.slot);
        hidden.sort_by_key(|a| a.slot);
        if self.ability_slot.is_none() {
            let selected = if !hidden.is_empty() && rng.next() % 128 == 0 {
                Some(hidden[(rng.next() % hidden.len() as u64) as usize])
            } else if !normal.is_empty() {
                Some(normal[(rng.next() % normal.len() as u64) as usize])
            } else {
                details.abilities.iter().min_by_key(|a| a.slot)
            };
            if let Some(a) = selected {
                self.ability_slot = Some(a.slot);
                self.ability_is_hidden = a.is_hidden;
            }
        }
        let chosen = details
            .abilities
            .iter()
            .find(|a| Some(a.slot) == self.ability_slot && a.is_hidden == self.ability_is_hidden)
            .or_else(|| {
                details
                    .abilities
                    .iter()
                    .find(|a| Some(a.slot) == self.ability_slot)
            })
            .or_else(|| normal.first().copied())
            .or_else(|| hidden.first().copied());
        self.ability_name = chosen.map(|a| a.name.clone());
        if let Some(a) = chosen {
            self.ability_slot = Some(a.slot);
            self.ability_is_hidden = a.is_hidden;
        }
        let moves = details.level_up_moves(self.level);
        self.moves = moves.into_iter().rev().take(4).collect();
        self.moves.reverse();
    }
    pub fn advance_growth(&mut self, candidate: u64, rarity: Rarity) {
        self.growth_tokens = self.growth_tokens.max(candidate).min(MAX_TOKENS);
        let progress = (self.growth_tokens as f64 / rarity.graduation_total() as f64).min(1.0);
        self.level = self
            .level
            .max(5 + (progress * 95.0).floor() as i32)
            .clamp(5, 100);
    }
    pub fn rebase_species(&mut self, old: Rarity, new: Rarity) {
        let progress = (self.growth_tokens as f64 / old.graduation_total() as f64).min(1.0);
        self.gender = None;
        self.ability_slot = None;
        self.ability_name = None;
        self.ability_is_hidden = false;
        self.moves.clear();
        self.growth_tokens = (progress * new.graduation_total() as f64).floor() as u64;
        self.advance_growth(self.growth_tokens, new);
    }
    pub fn sanitize(&mut self) {
        self.level = self.level.clamp(5, 100);
        self.growth_tokens = self.growth_tokens.min(MAX_TOKENS);
        for iv in &mut self.ivs {
            *iv = (*iv).clamp(0, 31);
        }
        self.moves.truncate(4);
        for item in &mut self.moves {
            item.name = item.name.chars().take(80).collect();
            item.learned_at_level = item.learned_at_level.clamp(0, 100);
        }
        if self.instance_id.is_empty() {
            self.instance_id = format!("recovered-{:016x}", self.seed);
        }
    }
}

pub fn nature_modifier(nature: &str, stat: &str) -> f64 {
    let pair = match nature {
        "lonely" => ("attack", "defense"),
        "brave" => ("attack", "speed"),
        "adamant" => ("attack", "special-attack"),
        "naughty" => ("attack", "special-defense"),
        "bold" => ("defense", "attack"),
        "relaxed" => ("defense", "speed"),
        "impish" => ("defense", "special-attack"),
        "lax" => ("defense", "special-defense"),
        "timid" => ("speed", "attack"),
        "hasty" => ("speed", "defense"),
        "jolly" => ("speed", "special-attack"),
        "naive" => ("speed", "special-defense"),
        "modest" => ("special-attack", "attack"),
        "mild" => ("special-attack", "defense"),
        "quiet" => ("special-attack", "speed"),
        "rash" => ("special-attack", "special-defense"),
        "calm" => ("special-defense", "attack"),
        "gentle" => ("special-defense", "defense"),
        "sassy" => ("special-defense", "speed"),
        "careful" => ("special-defense", "special-attack"),
        _ => return 1.0,
    };
    if stat == pair.0 {
        1.1
    } else if stat == pair.1 {
        0.9
    } else {
        1.0
    }
}

pub fn computed_stats(
    details: &Details,
    profile: &Profile,
    nature: Option<&str>,
) -> BTreeMap<String, i32> {
    STAT_ORDER
        .into_iter()
        .enumerate()
        .filter_map(|(i, name)| {
            let base = *details.base_stats.get(name)?;
            let neutral = ((2 * base + profile.ivs[i]) * profile.level) / 100;
            let value = if name == "hp" {
                if details.species_id == 292 {
                    1
                } else {
                    neutral + profile.level + 10
                }
            } else {
                ((neutral + 5) as f64 * nature.map_or(1.0, |n| nature_modifier(n, name))).floor()
                    as i32
            };
            Some((name.to_owned(), value))
        })
        .collect()
}
struct ProfileRng(u64);
impl ProfileRng {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }
}

/// An optional profile is recoverable metadata; damage must not discard its Pokémon.
pub fn decode_optional<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Profile>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}
impl super::Companion {
    pub fn enrich_profiles(&mut self, details: &Details) {
        self.profile_details
            .insert(details.species_id, details.clone());
        for mon in self.active.iter_mut().chain(self.collection.iter_mut()) {
            if mon.species_id() != details.species_id {
                continue;
            }
            if let Some(profile) = &mut mon.profile {
                profile.enrich(details);
            }
        }
    }
}

impl super::Companion {
    /// Profiles describe an individual's current/final form, not earlier forms in its ancestry.
    pub fn individuals(&self, species_id: u16) -> Vec<&super::Mon> {
        let mut archived: Vec<_> = self
            .collection
            .iter()
            .filter(|mon| mon.species_id() == species_id)
            .collect();
        archived.sort_by(|a, b| b.caught_at.cmp(&a.caught_at));
        self.active
            .iter()
            .filter(|mon| mon.species_id() == species_id)
            .chain(archived)
            .collect()
    }
}

impl super::Companion {
    pub fn individuals_for_form(
        &self,
        species_id: u16,
        form: Option<&super::forms::Form>,
    ) -> Vec<&super::Mon> {
        let form = super::forms::resolved(species_id, form);
        self.individuals(species_id)
            .into_iter()
            .filter(|mon| {
                mon.profile.is_some()
                    && super::forms::resolved(species_id, mon.form.as_ref()) == form
            })
            .collect()
    }
}
