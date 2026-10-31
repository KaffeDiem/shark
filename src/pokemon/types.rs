//! Classic Pokémon type colors used by detail badges.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Type {
    Normal,
    Fire,
    Water,
    Electric,
    Grass,
    Ice,
    Fighting,
    Poison,
    Ground,
    Flying,
    Psychic,
    Bug,
    Rock,
    Ghost,
    Dragon,
    Dark,
    Steel,
    Fairy,
}
impl Type {
    pub const ALL: [Self; 18] = [
        Self::Normal,
        Self::Fire,
        Self::Water,
        Self::Electric,
        Self::Grass,
        Self::Ice,
        Self::Fighting,
        Self::Poison,
        Self::Ground,
        Self::Flying,
        Self::Psychic,
        Self::Bug,
        Self::Rock,
        Self::Ghost,
        Self::Dragon,
        Self::Dark,
        Self::Steel,
        Self::Fairy,
    ];
    pub fn name(self) -> &'static str {
        [
            "normal", "fire", "water", "electric", "grass", "ice", "fighting", "poison", "ground",
            "flying", "psychic", "bug", "rock", "ghost", "dragon", "dark", "steel", "fairy",
        ][self as usize]
    }
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|t| t.name().eq_ignore_ascii_case(name))
    }
    pub fn color(self) -> u32 {
        [
            0xAAAA99, 0xFF4422, 0x3399FF, 0xFFCC33, 0x77CC55, 0x66CCFF, 0xBB5544, 0xAA5599,
            0xDDBB55, 0x8899FF, 0xFF5599, 0xAABB22, 0xBBAA66, 0x6666BB, 0x7766EE, 0x775544,
            0xAAAABB, 0xEE99EE,
        ][self as usize]
    }
}
pub fn color(name: &str) -> u32 {
    Type::parse(name).map_or(0x808080, Type::color)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classic_type_palette_and_unknown_fallback() {
        assert_eq!(color("fire"), 0xFF4422);
        assert_eq!(color("water"), 0x3399FF);
        assert_eq!(color("fairy"), 0xEE99EE);
        assert_eq!(color("Electric"), 0xFFCC33);
        assert_eq!(color("GRASS"), 0x77CC55);
        assert_eq!(
            Type::ALL.map(Type::name),
            [
                "normal", "fire", "water", "electric", "grass", "ice", "fighting", "poison",
                "ground", "flying", "psychic", "bug", "rock", "ghost", "dragon", "dark", "steel",
                "fairy"
            ]
        );
        for name in ["stellar", "shadow", "unknown", ""] {
            assert!(Type::parse(name).is_none());
            assert_eq!(color(name), 0x808080)
        }
    }
}
