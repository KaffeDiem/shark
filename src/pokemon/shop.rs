//! Shop presentation and purchase gates share the same prices as mutations.
use super::{Companion, Rarity};
impl Companion {
    pub fn rolls_shiny(roll: u64, charm_owned: bool) -> bool {
        roll % (if charm_owned { 48 } else { 64 }) == 0
    }
}

pub fn decode_egg_tier<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Rarity>, D::Error> {
    use serde::Deserialize;
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}
