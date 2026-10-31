//! Shop presentation and purchase gates share the same prices as mutations.
use super::{Companion, Item, Rarity};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShopEntry {
    Item(Item),
    Egg(Option<Rarity>),
}
impl Companion {
    pub fn item_count(&self, item: Item) -> u64 {
        match item {
            Item::RareCandy => self.inventory.candies,
            Item::Mint => self.inventory.mints,
            Item::ShinyCharm => u64::from(self.inventory.shiny_charm),
        }
    }
    pub fn max_buy_count(&self, item: Item) -> u64 {
        if item.is_passive() && self.item_count(item) > 0 {
            return 0;
        }
        let affordable = self.balance() / self.price(item).max(1);
        if item.is_passive() {
            affordable.min(1)
        } else {
            affordable.min(u64::MAX - self.item_count(item))
        }
    }
    pub fn can_buy(&self, item: Item) -> bool {
        self.max_buy_count(item) > 0
    }
    pub fn can_buy_egg(&self, tier: Option<Rarity>) -> bool {
        self.active.is_some()
            && !matches!(tier, Some(Rarity::Common | Rarity::Legendary))
            && self.balance() >= self.egg_price(tier)
    }
    pub fn purchasable_items(&self) -> Vec<Item> {
        let mut items = vec![Item::RareCandy, Item::Mint, Item::ShinyCharm];
        items.sort_by_key(|&item| {
            (
                item.is_passive() && self.item_count(item) > 0,
                self.price(item),
            )
        });
        items
    }
    pub fn shop_entries(&self) -> Vec<ShopEntry> {
        let mut entries: Vec<_> = self
            .purchasable_items()
            .into_iter()
            .map(ShopEntry::Item)
            .chain(
                [None, Some(Rarity::Uncommon), Some(Rarity::Rare)]
                    .into_iter()
                    .map(ShopEntry::Egg),
            )
            .collect();
        entries.sort_by_key(|entry| match *entry {
            ShopEntry::Item(item) => (
                item.is_passive() && self.item_count(item) > 0,
                self.price(item),
            ),
            ShopEntry::Egg(tier) => (false, self.egg_price(tier)),
        });
        entries
    }
}

#[derive(Clone, Debug, Default)]
pub struct Feedback {
    pub candy_seq: u64,
    pub candy_amount: u64,
    pub mint_seq: u64,
    pub mint_nature: Option<String>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct CandyPlan {
    pub count: u64,
    pub evolves: bool,
    pub graduates: bool,
    pub carryover_xp: u64,
    pub discarded_xp: u64,
}
impl Companion {
    pub fn shiny_denominator(&self) -> u64 {
        if self.inventory.shiny_charm { 48 } else { 64 }
    }
    pub fn rolls_shiny(roll: u64, charm_owned: bool) -> bool {
        roll % (if charm_owned { 48 } else { 64 }) == 0
    }
    pub fn max_candy_use_count(&self) -> u64 {
        let Some(mon) = &self.active else {
            return 0;
        };
        if mon.ditto_disguise.is_some()
            && !mon.ditto_revealed
            && mon.tokens >= mon.threshold(self.growth_difficulty)
        {
            return 0;
        }
        let remaining = (mon.stage..mon.path.len())
            .map(|stage| {
                super::Balance::scaled(
                    super::Balance::phase_threshold(
                        mon.line.rarity,
                        mon.path.len(),
                        stage,
                        if mon.growth_boost { 2 } else { 1 },
                    ),
                    self.growth_difficulty,
                )
                .max(1)
            })
            .fold(0u64, u64::saturating_add)
            .saturating_sub(mon.tokens);
        (remaining / super::CANDY_XP + u64::from(remaining % super::CANDY_XP != 0))
            .min(self.inventory.candies)
    }
    pub fn plan_candy_use(&self, requested: u64) -> Option<CandyPlan> {
        let mon = self.active.as_ref()?;
        let count = requested.min(self.max_candy_use_count());
        if count == 0 {
            return None;
        }
        let mut xp = mon
            .tokens
            .saturating_add(count.saturating_mul(super::CANDY_XP));
        let mut stage = mon.stage;
        while stage < mon.path.len() {
            let threshold = super::Balance::scaled(
                super::Balance::phase_threshold(
                    mon.line.rarity,
                    mon.path.len(),
                    stage,
                    if mon.growth_boost { 2 } else { 1 },
                ),
                self.growth_difficulty,
            )
            .max(1);
            if xp < threshold {
                break;
            }
            xp -= threshold;
            stage += 1;
        }
        let graduates = stage >= mon.path.len();
        Some(CandyPlan {
            count,
            evolves: stage > mon.stage
                && (!graduates || mon.path.len().saturating_sub(mon.stage) > 1),
            graduates,
            carryover_xp: if graduates || stage == mon.stage {
                0
            } else {
                xp
            },
            discarded_xp: if graduates { xp } else { 0 },
        })
    }
    pub fn owned_items(&self) -> Vec<(Item, u64)> {
        [Item::RareCandy, Item::Mint, Item::ShinyCharm]
            .into_iter()
            .filter_map(|item| {
                let count = self.item_count(item);
                (count > 0).then_some((item, count))
            })
            .collect()
    }
    pub fn consume_candy_feedback(&mut self) {
        self.feedback.candy_amount = 0;
    }
    pub fn consume_mint_feedback(&mut self) {
        self.feedback.mint_nature = None;
    }
}
impl Item {
    pub fn sprite_name(self) -> Option<&'static str> {
        match self {
            Self::RareCandy => Some("rare-candy"),
            Self::Mint => None,
            Self::ShinyCharm => Some("shiny-charm"),
        }
    }
}

pub fn decode_egg_tier<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Rarity>, D::Error> {
    use serde::Deserialize;
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}
