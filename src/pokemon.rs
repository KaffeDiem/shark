//! Rust port of PokeTokenBar's companion economy and growth model.
//! Upstream: chattymin/PokeTokenBar, MIT; see licenses/PokeTokenBar-MIT.txt.
//! The model is independent of usage providers and UI. All network work belongs on a worker.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub mod animation;
pub mod api;
pub mod collection;
pub mod ditto;
pub mod evolution;
pub mod format;
pub mod forms;
pub mod interface_labels;
pub mod limits;
pub mod localization;
pub mod names;
pub mod names_client;
pub mod profile;
pub mod shop;
pub mod sprites;
pub mod storage;
pub mod types;
pub use api::PokeApi;
pub use shop::ShopEntry;
#[cfg(test)]
mod lifecycle_tests;
#[cfg(test)]
mod qa_tests;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rarity {
    #[default]
    Common,
    Uncommon,
    Rare,
    Legendary,
}
impl Rarity {
    pub fn rank(self) -> u8 {
        self as u8
    }
    pub fn capture_rate_ceiling(self) -> Option<u16> {
        match self {
            Self::Common => Some(255),
            Self::Uncommon => Some(120),
            Self::Rare => Some(45),
            Self::Legendary => None,
        }
    }
    pub fn includes(self, capture_rate: u16) -> bool {
        self.capture_rate_ceiling()
            .is_some_and(|n| capture_rate <= n)
    }
    pub fn from_capture_rate(rate: u16, legendary: bool, mythical: bool) -> Self {
        if legendary || mythical {
            Self::Legendary
        } else if rate <= 45 {
            Self::Rare
        } else if rate <= 120 {
            Self::Uncommon
        } else {
            Self::Common
        }
    }
    pub fn graduation_total(self) -> u64 {
        match self {
            Self::Common => 750_000_000,
            Self::Uncommon => 1_875_000_000,
            Self::Rare => 3_000_000_000,
            Self::Legendary => 6_000_000_000,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Common => "Common",
            Self::Uncommon => "Uncommon",
            Self::Rare => "Rare",
            Self::Legendary => "Legendary",
        }
    }
}
pub struct Balance;
impl Balance {
    pub const EGG_HATCH_THRESHOLD: u64 = 5_000_000;
    pub const REPEAT_GROWTH_MULTIPLIER: u64 = 2;
    pub fn phase_threshold(
        rarity: Rarity,
        forms: usize,
        stage: usize,
        growth_multiplier: u64,
    ) -> u64 {
        let forms = forms.max(1) as f64;
        let standard = (rarity.graduation_total() as f64 * (stage.saturating_add(1) as f64)
            / (forms * (forms + 1.) / 2.))
            .round();
        (standard / growth_multiplier.max(1) as f64).round().max(1.) as u64
    }
    pub fn clamp_difficulty(value: f64) -> f64 {
        if value.is_finite() {
            value.clamp(0.1, 2.)
        } else {
            1.
        }
    }
    pub fn scaled(base: u64, difficulty: f64) -> u64 {
        (base as f64 * Self::clamp_difficulty(difficulty)).round() as u64
    }
    pub fn difficulty_position(value: f64) -> f64 {
        (Self::clamp_difficulty(value) / 0.1).ln() / 20_f64.ln()
    }
    pub fn difficulty_at_position(position: f64) -> f64 {
        let p = if position.is_finite() {
            position.clamp(0., 1.)
        } else {
            Self::difficulty_position(1.)
        };
        if (p - Self::difficulty_position(1.)).abs() < 0.01 {
            1.
        } else {
            Self::snap_difficulty(0.1 * 20_f64.powf(p))
        }
    }
    pub fn snap_difficulty(value: f64) -> f64 {
        if !value.is_finite() {
            return 1.;
        }
        if value <= 0. {
            return 0.1;
        }
        let magnitude = 10_f64.powf(value.log10().floor() - 1.);
        (value / magnitude).round() * magnitude
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Item {
    RareCandy,
    Mint,
    ShinyCharm,
}
impl Item {
    pub fn price(self) -> u64 {
        match self {
            Self::RareCandy => 500_000_000,
            Self::Mint => 100_000_000,
            Self::ShinyCharm => 3_000_000_000,
        }
    }
    pub fn is_passive(self) -> bool {
        self == Self::ShinyCharm
    }
}
pub struct FreshEgg;
impl FreshEgg {
    pub fn price(tier: Option<Rarity>) -> u64 {
        tier.map_or(1_000_000_000, |t| {
            (1_000_000_000_f64 * t.graduation_total() as f64
                / Rarity::Common.graduation_total() as f64)
                .round() as u64
        })
    }
}
pub const CANDY_XP: u64 = 100_000_000;
pub const NATURES: [&str; 25] = [
    "Hardy", "Lonely", "Brave", "Adamant", "Naughty", "Bold", "Docile", "Relaxed", "Impish", "Lax",
    "Timid", "Hasty", "Serious", "Jolly", "Naive", "Modest", "Mild", "Quiet", "Bashful", "Rash",
    "Calm", "Gentle", "Sassy", "Careful", "Quirky",
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvoNode {
    pub species_id: u16,
    #[serde(default)]
    pub children: Vec<EvoNode>,
}
impl EvoNode {
    pub fn depth(&self) -> usize {
        1 + self.children.iter().map(Self::depth).max().unwrap_or(0)
    }
    pub fn node(&self, id: u16) -> Option<&Self> {
        if self.species_id == id {
            Some(self)
        } else {
            self.children.iter().find_map(|n| n.node(id))
        }
    }
    pub fn final_ids(&self) -> Vec<u16> {
        if self.children.is_empty() {
            vec![self.species_id]
        } else {
            self.children.iter().flat_map(Self::final_ids).collect()
        }
    }
    pub fn keeping_animated_sprites(&self) -> Option<Self> {
        (1..=649).contains(&self.species_id).then(|| Self {
            species_id: self.species_id,
            children: self
                .children
                .iter()
                .filter_map(Self::keeping_animated_sprites)
                .collect(),
        })
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvoLine {
    pub base_id: u16,
    pub tree: EvoNode,
    pub rarity: Rarity,
    #[serde(default)]
    pub names: BTreeMap<u16, BTreeMap<String, String>>,
}
impl EvoLine {
    pub fn name(&self, id: u16) -> String {
        self.localized_name(id, "en")
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mon {
    pub line: EvoLine,
    #[serde(default, alias = "unown_form")]
    pub form: Option<forms::Form>,
    pub path: Vec<u16>,
    #[serde(default, deserialize_with = "decode_stage")]
    pub stage: usize,
    pub tokens: u64,
    #[serde(default)]
    pub shiny: bool,
    #[serde(default)]
    pub ditto_disguise: Option<u16>,
    #[serde(default)]
    pub ditto_revealed: bool,
    #[serde(default = "default_nature")]
    pub nature: String,
    #[serde(default)]
    pub names_version: Option<u32>,
    #[serde(default, deserialize_with = "profile::decode_optional")]
    pub profile: Option<profile::Profile>,
    #[serde(default)]
    pub graduated: bool,
    #[serde(default)]
    pub released: bool,
    #[serde(default)]
    pub growth_boost: bool,
    #[serde(default)]
    pub caught_at: u64,
}
impl Mon {
    pub fn displays_shiny(&self) -> bool {
        self.shiny && (self.ditto_disguise.is_none() || self.ditto_revealed)
    }
    pub fn species_id(&self) -> u16 {
        self.path
            .get(self.stage)
            .copied()
            .or_else(|| self.path.last().copied())
            .unwrap_or(self.line.base_id)
    }
    pub fn name(&self) -> String {
        let label = forms::label(self.species_id(), self.form.as_ref(), None, "en");
        forms::display_name(
            &self.line.name(self.species_id()),
            self.species_id(),
            self.form.as_ref(),
            label.as_deref(),
        )
    }
    pub fn reached_species(&self) -> impl Iterator<Item = u16> + '_ {
        self.path.iter().copied().take(self.stage.saturating_add(1))
    }
    pub fn reconcile_profile(&mut self, difficulty: f64) {
        let completed = (0..self.stage.min(self.path.len())).fold(0u64, |total, stage| {
            total.saturating_add(Balance::phase_threshold(
                self.line.rarity,
                self.path.len(),
                stage,
                1,
            ))
        });
        let standard = Balance::phase_threshold(self.line.rarity, self.path.len(), self.stage, 1);
        let fraction = (self.tokens as f64 / self.threshold(difficulty) as f64).clamp(0., 1.);
        let candidate = completed
            .saturating_add((standard as f64 * fraction).floor() as u64)
            .min(self.line.rarity.graduation_total());
        if self.profile.is_none() {
            let seed = profile::migration_seed(&format!(
                "{}:{}:{}",
                self.line.base_id, self.caught_at, self.nature
            ));
            self.profile = Some(profile::Profile::generate(
                seed,
                format!("legacy-{seed:016x}"),
            ));
        }
        if let Some(profile) = &mut self.profile {
            profile.advance_growth(candidate, self.line.rarity);
        }
    }
    pub fn threshold(&self, difficulty: f64) -> u64 {
        Balance::scaled(
            Balance::phase_threshold(
                self.line.rarity,
                self.path.len(),
                self.stage,
                if self.growth_boost { 2 } else { 1 },
            ),
            difficulty,
        )
        .max(1)
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Inventory {
    pub candies: u64,
    pub mints: u64,
    pub shiny_charm: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UsageBaseline {
    pub day: String,
    pub total: u64,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LimitBaseline {
    pub epoch: Option<String>,
    pub claimed: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Companion {
    #[serde(default, deserialize_with = "decode_active")]
    pub active: Option<Mon>,
    pub egg_tokens: u64,
    pub pending_line: Option<EvoLine>,
    #[serde(alias = "pending_unown_form")]
    pub pending_form: Option<forms::Form>,
    #[serde(default, deserialize_with = "decode_collection")]
    pub collection: Vec<Mon>,
    pub lifetime_tokens: u64,
    pub spent_tokens: u64,
    pub inventory: Inventory,
    #[serde(skip)]
    pub feedback: shop::Feedback,
    #[serde(skip)]
    pub profile_details: BTreeMap<u16, profile::Details>,
    pub growth_difficulty: f64,
    pub shop_difficulty: f64,
    pub baselines: BTreeMap<String, UsageBaseline>,
    pub limit_baselines: BTreeMap<String, LimitBaseline>,
    #[serde(default, deserialize_with = "shop::decode_egg_tier")]
    pub egg_tier: Option<Rarity>,
    pub representative: Option<u16>,
    #[serde(alias = "representative_unown_form")]
    pub representative_form: Option<forms::Form>,
    pub representative_shiny: Option<bool>,
    /// Persist the random sequence so reopening the app cannot reroll an existing individual.
    pub random_seed: u64,
}
impl Default for Companion {
    fn default() -> Self {
        Self {
            active: None,
            egg_tokens: 0,
            pending_line: None,
            pending_form: None,
            collection: vec![],
            lifetime_tokens: 0,
            spent_tokens: 0,
            inventory: Inventory::default(),
            feedback: shop::Feedback::default(),
            profile_details: BTreeMap::new(),
            growth_difficulty: 1.,
            shop_difficulty: 1.,
            baselines: BTreeMap::new(),
            limit_baselines: BTreeMap::new(),
            egg_tier: None,
            representative: None,
            representative_form: None,
            representative_shiny: None,
            random_seed: seed(),
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Hatched(String),
    Evolved(String),
    Graduated(String),
}
impl std::fmt::Display for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Hatched(n) => write!(f, "{n} hatched!"),
            Self::Evolved(n) => write!(f, "Evolved into {n}!"),
            Self::Graduated(n) => write!(f, "{n} joined your Pokédex!"),
        }
    }
}
pub(crate) fn seed() -> u64 {
    use std::io::Read;
    let mut bytes = [0; 8];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .is_ok()
    {
        u64::from_ne_bytes(bytes)
    } else {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64
    }
}
pub(crate) fn next_random(state: &mut u64) -> u64 {
    // SplitMix64, reproducible and nonzero-seed safe.
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}
impl Companion {
    /// Normalize externally edited/imported saves before arithmetic or rendering.
    pub fn sanitize(&mut self) {
        const MAX: u64 = 1_000_000_000_000_000;
        self.lifetime_tokens = self.lifetime_tokens.min(MAX);
        self.spent_tokens = self.spent_tokens.min(MAX);
        self.egg_tokens = self.egg_tokens.min(MAX);
        self.inventory.candies = self.inventory.candies.min(MAX / CANDY_XP);
        self.inventory.mints = self.inventory.mints.min(MAX);
        let previous_difficulty = self.growth_difficulty;
        self.growth_difficulty = Balance::clamp_difficulty(self.growth_difficulty);
        self.shop_difficulty = Balance::clamp_difficulty(self.shop_difficulty);
        if self.active.is_some()
            || self.pending_line.as_ref().is_some_and(|p| {
                !(1..=649).contains(&p.base_id) || self.egg_tier.is_some_and(|tier| p.rarity < tier)
            })
        {
            self.pending_line = None;
        }
        self.pending_form = self
            .pending_line
            .as_ref()
            .and_then(|p| forms::resolved_line(p.base_id, self.pending_form.as_ref()));
        for baseline in self.baselines.values_mut() {
            baseline.total = baseline.total.min(MAX);
        }
        fn tree(
            node: &mut EvoNode,
            seen: &mut std::collections::BTreeSet<u16>,
            depth: usize,
        ) -> bool {
            if depth > 8 || !(1..=649).contains(&node.species_id) || !seen.insert(node.species_id) {
                return false;
            }
            node.children
                .retain_mut(|child| tree(child, seen, depth + 1));
            true
        }
        fn mon(mon: &mut Mon) -> bool {
            if !tree(&mut mon.line.tree, &mut Default::default(), 0) {
                return false;
            }
            mon.line.base_id = mon.line.tree.species_id;
            let current = mon.species_id();
            let reached = mon.reached_species().collect::<Vec<_>>();
            let mut seen = std::collections::BTreeSet::new();
            mon.path.retain(|id| seen.insert(*id));
            let mut node = &mon.line.tree;
            let mut valid = vec![node.species_id];
            for id in mon.path.iter().skip(1) {
                let Some(child) = node.children.iter().find(|n| n.species_id == *id) else {
                    break;
                };
                valid.push(*id);
                node = child;
            }
            if !mon.released {
                while let Some(child) = node.children.first() {
                    valid.push(child.species_id);
                    node = child;
                }
            }
            mon.path = valid;
            mon.stage = mon
                .path
                .iter()
                .position(|id| *id == current)
                .or_else(|| {
                    reached
                        .iter()
                        .rev()
                        .find_map(|id| mon.path.iter().position(|valid| valid == id))
                })
                .unwrap_or(0);
            mon.tokens = mon.tokens.min(MAX);
            mon.form = forms::resolved_line(mon.line.base_id, mon.form.as_ref());
            mon.nature = NATURES
                .iter()
                .find(|name| name.eq_ignore_ascii_case(&mon.nature))
                .copied()
                .unwrap_or("Hardy")
                .into();
            if let Some(profile) = &mut mon.profile {
                let missing_identity = profile.instance_id.is_empty();
                profile.sanitize();
                // Batch recovery assigns a unique deterministic ID without changing
                // this otherwise recoverable profile's IVs or species metadata.
                if missing_identity {
                    profile.instance_id.clear();
                }
            }
            true
        }
        if self.active.as_mut().is_some_and(|m| !mon(m)) {
            self.active = None;
        }
        self.collection.retain_mut(mon);
        if previous_difficulty.is_finite()
            && previous_difficulty > 0.
            && previous_difficulty != self.growth_difficulty
        {
            // Old saves may predate today's difficulty range. Reprice their raw
            // phase credits once, preserving the fraction before the old value is lost.
            let base = self
                .active
                .as_ref()
                .map_or(Balance::EGG_HATCH_THRESHOLD, |mon| {
                    Balance::phase_threshold(
                        mon.line.rarity,
                        mon.path.len(),
                        mon.stage,
                        if mon.growth_boost { 2 } else { 1 },
                    )
                });
            let old_threshold = (base as f64 * previous_difficulty).round().max(1.) as u64;
            let new_threshold = self.progress().1;
            let reprice = |tokens: u64| {
                let scaled =
                    (tokens as f64 / old_threshold as f64 * new_threshold as f64).round() as u64;
                if tokens < old_threshold {
                    scaled.min(new_threshold.saturating_sub(1)).min(MAX)
                } else {
                    scaled.min(MAX)
                }
            };
            if let Some(mon) = &mut self.active {
                mon.tokens = reprice(mon.tokens);
            } else {
                self.egg_tokens = reprice(self.egg_tokens);
            }
        }
        // Reserve every persisted identity before generating any recovered individual.
        // Two old catches can share species, nature, and second-resolution timestamps.
        let mut profile_ids = self
            .active
            .iter()
            .chain(self.collection.iter())
            .filter_map(|mon| mon.profile.as_ref())
            .filter(|profile| !profile.instance_id.is_empty())
            .map(|profile| profile.instance_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        fn recover_identity(
            mon: &mut Mon,
            used: &mut std::collections::BTreeSet<String>,
            generated: bool,
        ) {
            let Some(profile) = &mut mon.profile else {
                return;
            };
            if !generated && !profile.instance_id.is_empty() {
                return;
            }
            let base = if generated {
                profile.instance_id.clone()
            } else {
                format!("recovered-{:016x}", profile.seed)
            };
            let mut ordinal = 0u64;
            let id = loop {
                let candidate = if ordinal == 0 {
                    base.clone()
                } else {
                    format!("{base}-{ordinal}")
                };
                if used.insert(candidate.clone()) {
                    break candidate;
                }
                ordinal += 1;
            };
            if generated && ordinal > 0 {
                let seed = profile::migration_seed(&format!("{:016x}:{ordinal}", profile.seed));
                let mut recovered = profile::Profile::generate(seed, id);
                recovered.advance_growth(profile.growth_tokens, mon.line.rarity);
                *profile = recovered;
            } else {
                profile.instance_id = id;
            }
        }
        if let Some(active) = &mut self.active {
            let generated = active.profile.is_none();
            active.reconcile_profile(self.growth_difficulty);
            recover_identity(active, &mut profile_ids, generated);
        }
        for mon in &mut self.collection {
            // Released individuals store only their reached ancestry. Reconstruct legacy
            // metadata once; repricing an existing profile against that shorter path would
            // invent completed growth on every reload.
            let generated = mon.profile.is_none();
            if generated {
                mon.reconcile_profile(self.growth_difficulty);
            }
            recover_identity(mon, &mut profile_ids, generated);
            if mon.graduated {
                if let Some(profile) = &mut mon.profile {
                    profile.advance_growth(mon.line.rarity.graduation_total(), mon.line.rarity);
                }
            }
        }
        if self.active.is_some()
            || matches!(self.egg_tier, Some(Rarity::Common | Rarity::Legendary))
        {
            self.egg_tier = None;
        }
        if let Some(id) = self.representative {
            if !self
                .collection
                .iter()
                .chain(self.active.iter())
                .any(|m| m.reached_species().any(|s| s == id))
            {
                self.representative = None;
            }
        }
        if let (Some(id), Some(shiny)) = (self.representative, self.representative_shiny) {
            let form = forms::resolved(id, self.representative_form.as_ref());
            if !self.collection.iter().chain(self.active.iter()).any(|m| {
                m.displays_shiny() == shiny
                    && m.reached_species().any(|s| s == id)
                    && forms::resolved(id, m.form.as_ref()) == form
            }) {
                self.representative = None;
            }
        }
        if self.representative.is_none() {
            self.representative_form = None;
            self.representative_shiny = None;
        }
    }
    pub fn title(&self) -> String {
        self.active
            .as_ref()
            .map_or_else(|| "Token Egg".into(), Mon::name)
    }
    pub fn progress(&self) -> (u64, u64) {
        self.active.as_ref().map_or(
            (
                self.egg_tokens,
                Balance::scaled(Balance::EGG_HATCH_THRESHOLD, self.growth_difficulty).max(1),
            ),
            |m| (m.tokens, m.threshold(self.growth_difficulty)),
        )
    }
    pub fn ready_to_hatch(&self) -> bool {
        self.active.is_none() && self.progress().0 >= self.progress().1
    }
    pub fn is_high_value_companion(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|mon| mon.line.rarity == Rarity::Legendary || mon.displays_shiny())
    }
    pub fn min_rarity(&self) -> Option<Rarity> {
        self.egg_tier
    }
    pub fn balance(&self) -> u64 {
        self.lifetime_tokens.saturating_sub(self.spent_tokens)
    }
    pub fn price(&self, item: Item) -> u64 {
        Balance::scaled(item.price(), self.shop_difficulty)
    }
    pub fn egg_price(&self, tier: Option<Rarity>) -> u64 {
        Balance::scaled(FreshEgg::price(tier), self.shop_difficulty)
    }
    pub fn representative_subject(&self) -> Option<(u16, bool)> {
        if let Some(id) = self.representative {
            let owned = self
                .collection
                .iter()
                .chain(self.active.iter())
                .filter(|m| m.reached_species().any(|s| s == id))
                .collect::<Vec<_>>();
            if !owned.is_empty() {
                return Some((
                    id,
                    self.representative_shiny
                        .unwrap_or_else(|| owned.iter().any(|m| m.displays_shiny())),
                ));
            }
        }
        self.active
            .as_ref()
            .map(|m| (m.species_id(), m.displays_shiny()))
    }
    pub fn representative_sprite_form(&self) -> Option<forms::Form> {
        if let Some(id) = self.representative {
            if self.representative_shiny.is_some() {
                return forms::resolved(id, self.representative_form.as_ref());
            }
            let shiny = self.representative_subject().is_some_and(|(_, s)| s);
            return self
                .collection
                .iter()
                .chain(self.active.iter())
                .find(|m| m.displays_shiny() == shiny && m.reached_species().any(|s| s == id))
                .and_then(|m| forms::resolved(id, m.form.as_ref()));
        }
        self.active
            .as_ref()
            .and_then(|m| forms::resolved(m.species_id(), m.form.as_ref()))
    }
    pub fn pin_appearance(&mut self, species: u16, shiny: bool, form: Option<forms::Form>) -> bool {
        let resolved = forms::resolved(species, form.as_ref());
        if !self.collection.iter().chain(self.active.iter()).any(|m| {
            m.displays_shiny() == shiny
                && m.reached_species().any(|id| id == species)
                && forms::resolved(species, m.form.as_ref()) == resolved
        }) {
            return false;
        }
        self.representative = Some(species);
        self.representative_shiny = Some(shiny);
        self.representative_form = resolved;
        true
    }
    pub fn sprite_url(&self) -> Option<String> {
        self.representative_subject().map(|(id, shiny)| {
            forms::sprite_url(id, true, shiny, self.representative_sprite_form().as_ref())
        })
    }
    pub fn pin(&mut self, id: Option<u16>) -> bool {
        if id.is_some_and(|id| {
            !self
                .collection
                .iter()
                .chain(self.active.iter())
                .any(|m| m.reached_species().any(|s| s == id))
        }) {
            return false;
        }
        self.representative = id;
        self.representative_form = None;
        self.representative_shiny = None;
        true
    }
    /// First observation of each provider establishes an installation baseline. New dates count
    /// from zero; late results for older dates are ignored. Disabled scans still advance baselines.
    pub fn observe_usage(
        &mut self,
        provider: &str,
        day: &str,
        total: u64,
        enabled: bool,
    ) -> Vec<Event> {
        if provider.is_empty() || day.is_empty() {
            return vec![];
        }
        let Some(previous) = self.baselines.get_mut(provider) else {
            self.baselines.insert(
                provider.into(),
                UsageBaseline {
                    day: day.into(),
                    total,
                },
            );
            return vec![];
        };
        // ISO dates compare lexically; Shark's local-day keys are integer epochs.
        let order = match (day.parse::<i64>(), previous.day.parse::<i64>()) {
            (Ok(a), Ok(b)) => a.cmp(&b),
            _ => day.cmp(previous.day.as_str()),
        };
        if order.is_lt() {
            return vec![];
        }
        let delta = if order.is_gt() {
            total
        } else {
            total.saturating_sub(previous.total)
        };
        previous.day = day.into();
        previous.total = if order.is_gt() {
            total
        } else {
            previous.total.max(total)
        };
        if !enabled {
            return vec![];
        }
        self.lifetime_tokens = self.lifetime_tokens.saturating_add(delta);
        self.add_growth(delta)
    }
    pub fn add_growth(&mut self, delta: u64) -> Vec<Event> {
        let Some(m) = self.active.as_mut() else {
            self.egg_tokens = self.egg_tokens.saturating_add(delta);
            return vec![];
        };
        m.tokens = m.tokens.saturating_add(delta);
        let mut events = vec![];
        while let Some(mon) = self.active.as_mut() {
            let threshold = mon.threshold(self.growth_difficulty);
            if mon.tokens < threshold {
                mon.reconcile_profile(self.growth_difficulty);
                break;
            }
            if mon.ditto_disguise.is_some() && !mon.ditto_revealed {
                mon.reconcile_profile(self.growth_difficulty);
                break;
            }
            mon.tokens -= threshold;
            if mon.stage.saturating_add(1) < mon.path.len() {
                mon.stage += 1;
                mon.reconcile_profile(self.growth_difficulty);
                if let Some(details) = self.profile_details.get(&mon.species_id()) {
                    if let Some(profile) = &mut mon.profile {
                        profile.enrich(details);
                    }
                }
                events.push(Event::Evolved(mon.name()));
            } else {
                let mut mon = self.active.take().unwrap();
                mon.graduated = true;
                mon.tokens = 0;
                if let Some(profile) = &mut mon.profile {
                    profile.advance_growth(mon.line.rarity.graduation_total(), mon.line.rarity);
                }
                mon.caught_at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                if let Some(details) = self.profile_details.get(&mon.species_id()) {
                    if let Some(profile) = &mut mon.profile {
                        profile.enrich(details);
                    }
                }
                events.push(Event::Graduated(mon.name()));
                self.collection.push(mon);
                self.egg_tokens = 0;
                break;
            }
        }
        if let Some(mon) = self.active.as_mut() {
            if let (Some(details), Some(profile)) = (
                self.profile_details.get(&mon.species_id()),
                mon.profile.as_mut(),
            ) {
                profile.enrich(details);
            }
        }
        events
    }
    pub fn prepare_egg(&mut self, mut line: EvoLine) -> bool {
        if self.active.is_some() || self.egg_tier.is_some_and(|tier| line.rarity < tier) {
            return false;
        }
        let Some(tree) = line.tree.keeping_animated_sprites() else {
            return false;
        };
        if tree.species_id != line.base_id {
            return false;
        }
        line.tree = tree;
        if self
            .pending_line
            .as_ref()
            .is_some_and(|previous| previous.base_id == line.base_id)
        {
            return true;
        }
        let pool = forms::line_forms(line.base_id);
        let collected = self.collected_line_forms(line.base_id);
        self.pending_form = if pool.is_empty() {
            None
        } else {
            forms::roll(next_random(&mut self.random_seed), &pool, &collected)
        };
        self.pending_line = Some(line);
        true
    }
    pub fn hatch_prepared(&mut self) -> Vec<Event> {
        let Some(line) = self.pending_line.clone() else {
            return vec![];
        };
        self.hatch(line)
    }
    pub fn hatch(&mut self, line: EvoLine) -> Vec<Event> {
        self.hatch_with_disguise_roll(line, !cfg!(test))
    }
    pub fn hatch_with_disguise_roll(
        &mut self,
        mut line: EvoLine,
        roll_disguise: bool,
    ) -> Vec<Event> {
        if !self.ready_to_hatch() || self.egg_tier.is_some_and(|tier| line.rarity < tier) {
            return vec![];
        }
        let Some(tree) = line.tree.keeping_animated_sprites() else {
            return vec![];
        };
        if tree.species_id != line.base_id {
            return vec![];
        }
        line.tree = tree;
        let shiny = Self::rolls_shiny(
            next_random(&mut self.random_seed),
            self.inventory.shiny_charm,
        );
        let nature = NATURES[(next_random(&mut self.random_seed) % 25) as usize].into();
        let ditto_disguise = (roll_disguise
            && ditto::disguise_hit(
                line.rarity,
                line.tree.depth(),
                next_random(&mut self.random_seed),
            ))
        .then_some(line.base_id);
        let mut path = vec![];
        let mut node = &line.tree;
        loop {
            path.push(node.species_id);
            if node.children.is_empty() {
                break;
            }
            let fresh = node
                .children
                .iter()
                .filter(|child| {
                    child.final_ids().iter().any(|id| {
                        !self.collection.iter().any(|m| {
                            m.graduated && m.line.base_id == line.base_id && m.species_id() == *id
                        })
                    })
                })
                .collect::<Vec<_>>();
            let pool = if fresh.is_empty() {
                node.children.iter().collect::<Vec<_>>()
            } else {
                fresh
            };
            node = pool[(next_random(&mut self.random_seed) % pool.len() as u64) as usize];
        }
        let growth_boost = self
            .collection
            .iter()
            .any(|m| m.graduated && m.line.base_id == line.base_id);
        let prepared = self
            .pending_line
            .as_ref()
            .is_some_and(|p| p.base_id == line.base_id);
        let prepared_form = if prepared {
            forms::resolved_line(line.base_id, self.pending_form.as_ref())
        } else {
            None
        };
        self.pending_line = None;
        self.pending_form = None;
        let carry = self.egg_tokens.saturating_sub(self.progress().1);
        self.egg_tokens = 0;
        self.egg_tier = None;
        let profile_seed = next_random(&mut self.random_seed);
        let profile = Some(profile::Profile::generate(
            profile_seed,
            format!("{profile_seed:016x}"),
        ));
        let pool = forms::line_forms(line.base_id);
        let collected = self.collected_line_forms(line.base_id);
        let form = if prepared {
            prepared_form
        } else if pool.is_empty() {
            None
        } else {
            forms::roll(next_random(&mut self.random_seed), &pool, &collected)
        };
        self.active = Some(Mon {
            ditto_disguise,
            ditto_revealed: false,
            line,
            path,
            stage: 0,
            tokens: 0,
            shiny,
            nature,
            names_version: Some(names::VERSION),
            profile,
            form,
            graduated: false,
            released: false,
            growth_boost,
            caught_at: 0,
        });
        let mut events = vec![Event::Hatched(self.title())];
        events.extend(self.add_growth(carry));
        events
    }
    pub fn buy(&mut self, item: Item, quantity: u64) -> Result<(), String> {
        if quantity == 0 {
            return Err("Choose at least one item".into());
        }
        if item == Item::ShinyCharm && (self.inventory.shiny_charm || quantity != 1) {
            return Err("You can own one Shiny Charm".into());
        }
        let cost = self
            .price(item)
            .checked_mul(quantity)
            .ok_or("Quantity is too large")?;
        if cost > self.balance() {
            return Err("Not enough tokens".into());
        }
        match item {
            Item::RareCandy => {
                self.inventory.candies = self
                    .inventory
                    .candies
                    .checked_add(quantity)
                    .ok_or("Inventory is full")?;
            }
            Item::Mint => {
                self.inventory.mints = self
                    .inventory
                    .mints
                    .checked_add(quantity)
                    .ok_or("Inventory is full")?;
            }
            Item::ShinyCharm => self.inventory.shiny_charm = true,
        }
        self.spent_tokens = self
            .spent_tokens
            .checked_add(cost)
            .ok_or("Token total is too large")?;
        Ok(())
    }
    pub fn use_candy(&mut self, quantity: u64) -> Result<Vec<Event>, String> {
        let mon = self
            .active
            .as_ref()
            .ok_or("Hatch your egg before using candy")?;
        if quantity == 0 || self.inventory.candies == 0 {
            return Err("Not enough Rare Candy".into());
        }
        let remaining = (mon.stage..mon.path.len())
            .fold(0u64, |sum, stage| {
                sum.saturating_add(
                    Balance::scaled(
                        Balance::phase_threshold(
                            mon.line.rarity,
                            mon.path.len(),
                            stage,
                            if mon.growth_boost { 2 } else { 1 },
                        ),
                        self.growth_difficulty,
                    )
                    .max(1),
                )
            })
            .saturating_sub(mon.tokens);
        let remaining = if mon.ditto_disguise.is_some() && !mon.ditto_revealed {
            mon.threshold(self.growth_difficulty)
                .saturating_sub(mon.tokens)
        } else {
            remaining
        };
        let needed = remaining / CANDY_XP + u64::from(remaining % CANDY_XP != 0);
        if needed == 0 {
            return Err("Your Pokémon is ready to evolve".into());
        }
        let consumed = quantity.min(needed).min(self.inventory.candies);
        self.inventory.candies -= consumed;
        self.feedback.candy_seq = self.feedback.candy_seq.wrapping_add(1);
        self.feedback.candy_amount = consumed.saturating_mul(CANDY_XP);
        Ok(self.add_growth(consumed.saturating_mul(CANDY_XP)))
    }
    pub fn use_mint(&mut self) -> Result<(), String> {
        let mon = self
            .active
            .as_mut()
            .ok_or("Hatch your egg before using a mint")?;
        if self.inventory.mints == 0 {
            return Err("No mints in your Bag".into());
        }
        let choices = NATURES
            .iter()
            .filter(|&&n| !n.eq_ignore_ascii_case(&mon.nature))
            .collect::<Vec<_>>();
        mon.nature =
            (*choices[(next_random(&mut self.random_seed) % choices.len() as u64) as usize]).into();
        self.inventory.mints -= 1;
        self.feedback.mint_seq = self.feedback.mint_seq.wrapping_add(1);
        self.feedback.mint_nature = Some(mon.nature.clone());
        Ok(())
    }
    pub fn fresh_egg(&mut self, tier: Option<Rarity>) -> Result<(), String> {
        if matches!(tier, Some(Rarity::Common | Rarity::Legendary)) {
            return Err("This egg grade is not sold".into());
        }
        if self.active.is_none() {
            return Err("You already have an egg".into());
        }
        let cost = self.egg_price(tier);
        if cost > self.balance() {
            return Err("Not enough tokens".into());
        }
        self.spent_tokens += cost;
        if let Some(mut mon) = self.active.take() {
            mon.shiny = mon.displays_shiny();
            mon.released = true;
            mon.path.truncate(mon.stage.saturating_add(1));
            mon.caught_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            self.collection.push(mon);
        }
        self.egg_tokens = 0;
        self.pending_line = None;
        self.pending_form = None;
        self.egg_tier = tier;
        Ok(())
    }
    pub fn set_growth_difficulty(&mut self, value: f64) {
        let old = self.progress().1;
        self.growth_difficulty = Balance::clamp_difficulty(value);
        let new = self.progress().1;
        let scale = |n: u64| {
            let scaled = ((n as f64 / old as f64) * new as f64).round() as u64;
            if n < old {
                scaled.min(new.saturating_sub(1))
            } else {
                scaled
            }
        };
        if let Some(mon) = &mut self.active {
            mon.tokens = scale(mon.tokens);
        } else {
            self.egg_tokens = scale(self.egg_tokens);
        }
    }
    pub fn set_shop_difficulty(&mut self, value: f64) {
        self.shop_difficulty = Balance::clamp_difficulty(value);
    }
    /// One reward per limit epoch; dipping below 100% rearms providers without reset timestamps.
    pub fn observe_limit(
        &mut self,
        key: &str,
        epoch: Option<&str>,
        utilization: f64,
        weekly: bool,
        enabled: bool,
    ) -> u64 {
        if !utilization.is_finite() || key.is_empty() {
            return 0;
        }
        let Some(baseline) = self.limit_baselines.get_mut(key) else {
            self.limit_baselines.insert(
                key.into(),
                LimitBaseline {
                    epoch: epoch.map(str::to_owned),
                    claimed: utilization >= 100.,
                },
            );
            return 0;
        };
        if let Some(epoch) = epoch {
            if let Some(previous) = baseline.epoch.as_deref() {
                if let (Some(previous), Some(incoming)) =
                    (limits::parse_epoch(previous), limits::parse_epoch(epoch))
                {
                    // A delayed observation belongs to its old window, not the unclaimed
                    // current one. Ignore its utilization as well as its reset timestamp.
                    if incoming < previous {
                        return 0;
                    }
                }
            }
            match baseline.epoch.as_deref() {
                Some(previous) if limits::is_new_epoch(previous, epoch, weekly) => {
                    baseline.epoch = Some(epoch.into());
                    baseline.claimed = false;
                }
                None => baseline.epoch = Some(epoch.into()),
                _ => {}
            }
        }
        if utilization < 100. {
            // A temporarily missing reset time must not erase an existing epoch's
            // claim. Utilization-only rearming is reserved for timestamp-free providers.
            if epoch.is_none() && baseline.epoch.is_none() {
                baseline.claimed = false;
            }
            return 0;
        }
        if baseline.claimed {
            return 0;
        }
        baseline.claimed = true;
        if !enabled {
            return 0;
        }
        let grant = if weekly { 5 } else { 1 };
        self.inventory.candies = self.inventory.candies.saturating_add(grant);
        grant
    }
}

fn default_nature() -> String {
    "Hardy".into()
}

fn decode_stage<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<usize, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    if let Some(value) = value.as_u64() {
        return Ok(usize::try_from(value).unwrap_or(usize::MAX));
    }
    if let Some(value) = value.as_i64() {
        return Ok(value.max(0) as usize);
    }
    Err(serde::de::Error::custom("Pokémon stage must be an integer"))
}

fn decode_active<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Mon>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value::<Mon>(value)
        .ok()
        .filter(|m| !m.path.is_empty()))
}
fn decode_collection<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Mon>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| {
            let mut value = v.clone();
            if let Some(map) = value.as_object_mut() {
                if !map.contains_key("graduated")
                    && !map
                        .get("released")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                {
                    map.insert("graduated".into(), true.into());
                }
            }
            serde_json::from_value::<Mon>(value).ok()
        })
        .filter(|m| !m.path.is_empty())
        .collect())
}
