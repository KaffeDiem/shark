//! Appearance forms from PokeTokenBar's Gen-V sprite catalog (MIT).
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Form(pub String);
#[derive(Clone, Debug)]
pub struct Entry {
    pub form: Form,
    pub form_id: u16,
    pub pokemon_id: Option<u16>,
}
fn entry(name: &str, form_id: u16, pokemon_id: Option<u16>) -> Entry {
    Entry {
        form: Form(name.into()),
        form_id,
        pokemon_id,
    }
}
pub const SPECIES: &[u16] = &[
    201, 351, 386, 412, 413, 421, 422, 423, 479, 487, 492, 493, 550, 555, 585, 586, 641, 642, 645,
    646, 647, 648, 649,
];
pub fn entries(species_id: u16) -> Vec<Entry> {
    match species_id {
        201 => (0..28)
            .map(|i| {
                entry(
                    &match i {
                        26 => "exclamation".into(),
                        27 => "question".into(),
                        _ => ((b'a' + i as u8) as char).to_string(),
                    },
                    if i == 0 { 201 } else { 10000 + i },
                    None,
                )
            })
            .collect(),
        351 => vec![
            entry("normal", 351, None),
            entry("sunny", 10028, Some(10013)),
            entry("rainy", 10029, Some(10014)),
            entry("snowy", 10030, Some(10015)),
        ],
        386 => vec![
            entry("normal", 386, None),
            entry("attack", 10031, Some(10001)),
            entry("defense", 10032, Some(10002)),
            entry("speed", 10033, Some(10003)),
        ],
        412 => vec![
            entry("plant", 412, None),
            entry("sandy", 10034, None),
            entry("trash", 10035, None),
        ],
        413 => vec![
            entry("plant", 413, None),
            entry("sandy", 10036, Some(10004)),
            entry("trash", 10037, Some(10005)),
        ],
        421 => vec![entry("overcast", 421, None), entry("sunshine", 10038, None)],
        422 => vec![entry("west", 422, None), entry("east", 10039, None)],
        423 => vec![entry("west", 423, None), entry("east", 10040, None)],
        479 => vec![
            entry("normal", 479, None),
            entry("heat", 10058, Some(10008)),
            entry("wash", 10059, Some(10009)),
            entry("frost", 10060, Some(10010)),
            entry("fan", 10061, Some(10011)),
            entry("mow", 10062, Some(10012)),
        ],
        487 => vec![
            entry("altered", 487, None),
            entry("origin", 10063, Some(10007)),
        ],
        492 => vec![entry("land", 492, None), entry("sky", 10064, Some(10006))],
        493 => vec![
            entry("normal", 493, None),
            entry("fighting", 10045, None),
            entry("flying", 10047, None),
            entry("poison", 10052, None),
            entry("ground", 10050, None),
            entry("rock", 10054, None),
            entry("bug", 10041, None),
            entry("ghost", 10048, None),
            entry("steel", 10055, None),
            entry("fire", 10046, None),
            entry("water", 10056, None),
            entry("grass", 10049, None),
            entry("electric", 10044, None),
            entry("psychic", 10053, None),
            entry("ice", 10051, None),
            entry("dragon", 10043, None),
            entry("dark", 10042, None),
            entry("fairy", 10085, None),
        ],
        550 => vec![
            entry("red-striped", 550, None),
            entry("blue-striped", 10066, Some(10016)),
        ],
        555 => vec![
            entry("standard", 555, None),
            entry("zen", 10067, Some(10017)),
        ],
        585 => vec![
            entry("spring", 585, None),
            entry("summer", 10068, None),
            entry("autumn", 10069, None),
            entry("winter", 10070, None),
        ],
        586 => vec![
            entry("spring", 586, None),
            entry("summer", 10071, None),
            entry("autumn", 10072, None),
            entry("winter", 10073, None),
        ],
        641 => vec![
            entry("incarnate", 641, None),
            entry("therian", 10079, Some(10019)),
        ],
        642 => vec![
            entry("incarnate", 642, None),
            entry("therian", 10080, Some(10020)),
        ],
        645 => vec![
            entry("incarnate", 645, None),
            entry("therian", 10081, Some(10021)),
        ],
        646 => vec![
            entry("normal", 646, None),
            entry("black", 10082, Some(10022)),
            entry("white", 10083, Some(10023)),
        ],
        647 => vec![
            entry("ordinary", 647, None),
            entry("resolute", 10084, Some(10024)),
        ],
        648 => vec![
            entry("aria", 648, None),
            entry("pirouette", 10074, Some(10018)),
        ],
        649 => vec![
            entry("normal", 649, None),
            entry("douse", 10075, None),
            entry("shock", 10076, None),
            entry("burn", 10077, None),
            entry("chill", 10078, None),
        ],
        _ => vec![],
    }
}
pub fn forms(species: u16) -> Vec<Form> {
    entries(species).into_iter().map(|e| e.form).collect()
}
pub fn line_base(species: u16) -> u16 {
    match species {
        413 => 412,
        421 => 420,
        423 => 422,
        555 => 554,
        586 => 585,
        _ => species,
    }
}
pub fn form_species(base: u16) -> Vec<u16> {
    SPECIES
        .iter()
        .copied()
        .filter(|&s| line_base(s) == base)
        .collect()
}
pub fn line_forms(base: u16) -> Vec<Form> {
    let mut seen = BTreeSet::new();
    form_species(base)
        .into_iter()
        .flat_map(forms)
        .filter(|f| seen.insert(f.clone()))
        .collect()
}
pub fn resolved(species: u16, form: Option<&Form>) -> Option<Form> {
    let fs = forms(species);
    form.filter(|f| fs.contains(f))
        .cloned()
        .or_else(|| fs.first().cloned())
}
pub fn resolved_line(base: u16, form: Option<&Form>) -> Option<Form> {
    let fs = line_forms(base);
    form.filter(|f| fs.contains(f))
        .cloned()
        .or_else(|| fs.first().cloned())
}
pub fn is_default(species: u16, form: Option<&Form>) -> bool {
    forms(species).first().cloned() == resolved(species, form)
}
pub fn sort_order(species: u16, form: Option<&Form>) -> usize {
    form.and_then(|f| forms(species).iter().position(|v| v == f))
        .unwrap_or(0)
}
pub fn roll(random: u64, pool: &[Form], collected: &BTreeSet<Form>) -> Option<Form> {
    let total: u64 = pool
        .iter()
        .map(|f| if collected.contains(f) { 1 } else { 2 })
        .sum();
    if total == 0 {
        return None;
    }
    let mut ticket = random % total;
    for form in pool {
        let weight = if collected.contains(form) { 1 } else { 2 };
        if ticket < weight {
            return Some(form.clone());
        }
        ticket -= weight;
    }
    None
}
pub fn asset_name(species: u16, form: Option<&Form>) -> String {
    match resolved(species, form) {
        Some(f) if !is_default(species, Some(&f)) => format!("{species}-{}", f.0),
        _ => species.to_string(),
    }
}
pub fn static_asset_name(species: u16, form: Option<&Form>) -> String {
    let f = resolved(species, form);
    entries(species)
        .iter()
        .find(|e| Some(&e.form) == f.as_ref())
        .and_then(|e| e.pokemon_id)
        .map_or_else(|| asset_name(species, form), |id| id.to_string())
}
pub fn form_id(species: u16, form: Option<&Form>) -> Option<u16> {
    let f = resolved(species, form);
    entries(species)
        .iter()
        .find(|e| Some(&e.form) == f.as_ref())
        .map(|e| e.form_id)
}
pub fn name_resource(species: u16, form: Option<&Form>) -> Option<u16> {
    if species == 201 || ([351, 479, 646, 649].contains(&species) && is_default(species, form)) {
        None
    } else {
        form_id(species, form)
    }
}
pub fn label(
    species: u16,
    form: Option<&Form>,
    names: Option<&BTreeMap<String, String>>,
    language: &str,
) -> Option<String> {
    let form = resolved(species, form)?;
    if species == 201 {
        return Some(form.symbol());
    }
    if [351, 479, 646, 649].contains(&species) && is_default(species, Some(&form)) {
        return Some(
            match language {
                "ko" => "노말폼",
                "ja" => "ノーマルフォルム",
                "es" | "pt" => "Forma Normal",
                "fr" => "Forme Normale",
                "de" => "Normalform",
                "ru" => "Обычная форма",
                _ => "Normal Form",
            }
            .into(),
        );
    }
    if let Some(name) = names.and_then(|names| super::evolution::resolve_name(names, &[language])) {
        return Some(name);
    }
    Some(
        form.0
            .split('-')
            .map(|word| {
                let mut chars = word.chars();
                chars.next().map_or_else(String::new, |first| {
                    first.to_uppercase().collect::<String>() + chars.as_str()
                })
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

pub fn display_name(name: &str, species: u16, form: Option<&Form>, label: Option<&str>) -> String {
    let Some(f) = resolved(species, form) else {
        return name.into();
    };
    if species == 201 {
        return format!("{name} [{}]", f.symbol());
    }
    if !is_default(species, Some(&f)) {
        if let Some(label) = label.filter(|s| !s.is_empty()) {
            return format!("{name} [{label}]");
        }
    }
    name.into()
}
impl Form {
    pub fn symbol(&self) -> String {
        match self.0.as_str() {
            "exclamation" => "!".into(),
            "question" => "?".into(),
            _ => self.0.to_uppercase(),
        }
    }
}

pub fn sprite_url(species: u16, animated: bool, shiny: bool, form: Option<&Form>) -> String {
    let base = "https://raw.githubusercontent.com/PokeAPI/sprites/master/sprites/pokemon";
    let color = if shiny { "shiny/" } else { "" };
    if animated {
        format!(
            "{base}/versions/generation-v/black-white/animated/{color}{}.gif",
            asset_name(species, form)
        )
    } else {
        format!("{base}/{color}{}.png", static_asset_name(species, form))
    }
}
pub fn cache_key(species: u16, animated: bool, shiny: bool, form: Option<&Form>) -> String {
    format!(
        "{}-{}{}",
        asset_name(species, form),
        if shiny { "sh" } else { "" },
        if animated { "a" } else { "s" }
    )
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use super::*;
    fn f(s: &str) -> Form {
        Form(s.into())
    }
    // upstream: PokemonFormTests.swift::testCatalogListsDefaultFirstWithinTheHatchableRange
    #[test]
    fn testCatalogListsDefaultFirstWithinTheHatchableRange() {
        assert_eq!(SPECIES.len(), 23);
        for &species in SPECIES {
            let es = entries(species);
            assert!((1..=649).contains(&species));
            assert!(es.len() >= 2);
            assert_eq!(
                es.iter()
                    .map(|e| e.form.clone())
                    .collect::<BTreeSet<_>>()
                    .len(),
                es.len()
            );
            assert_eq!(es[0].form_id, species);
            assert!(es.iter().skip(1).all(|e| e.form_id > 10000));
        }
        assert_eq!(forms(493).len(), 18);
        assert_eq!(forms(492), vec![f("land"), f("sky")]);
        for s in [414, 172, 592] {
            assert!(forms(s).is_empty());
        }
    }
    // upstream: PokemonFormTests.swift::testLinesCarryFormsFromTheirBaseEvenWhenTheBaseLooksTheSame
    #[test]
    fn testLinesCarryFormsFromTheirBaseEvenWhenTheBaseLooksTheSame() {
        assert_eq!(form_species(412), vec![412, 413]);
        assert_eq!(line_forms(412), vec![f("plant"), f("sandy"), f("trash")]);
        assert_eq!(form_species(420), vec![421]);
        assert_eq!(line_forms(420), vec![f("overcast"), f("sunshine")]);
        assert_eq!(line_forms(554), vec![f("standard"), f("zen")]);
        assert!(line_forms(1).is_empty());
        assert_eq!(resolved_line(420, None), Some(f("overcast")));
        assert_eq!(resolved(420, Some(&f("sunshine"))), None);
    }
    // upstream: PokemonFormTests.swift::testFormResolvesPerSpeciesOfTheLine
    #[test]
    fn testFormResolvesPerSpeciesOfTheLine() {
        for s in [412, 413] {
            assert_eq!(resolved(s, Some(&f("sandy"))), Some(f("sandy")));
        }
        assert_eq!(resolved(414, Some(&f("sandy"))), None);
        assert_eq!(resolved(492, None), Some(f("land")));
        assert_eq!(resolved(492, Some(&f("future"))), Some(f("land")));
        assert_eq!(resolved(25, Some(&f("sky"))), None);
    }
    // upstream: PokemonFormTests.swift::testVarietyFormsUseTheVarietyPNGAndTheFormGIF
    #[test]
    fn testVarietyFormsUseTheVarietyPNGAndTheFormGIF() {
        let base = "https://raw.githubusercontent.com/PokeAPI/sprites/master/sprites/pokemon";
        assert_eq!(
            sprite_url(492, false, false, Some(&f("sky"))),
            format!("{base}/10006.png")
        );
        assert_eq!(
            sprite_url(492, false, true, Some(&f("sky"))),
            format!("{base}/shiny/10006.png")
        );
        assert_eq!(
            sprite_url(492, true, true, Some(&f("sky"))),
            format!("{base}/versions/generation-v/black-white/animated/shiny/492-sky.gif")
        );
        assert_eq!(
            sprite_url(412, false, false, Some(&f("sandy"))),
            format!("{base}/412-sandy.png")
        );
        assert_eq!(
            sprite_url(492, false, false, Some(&f("land"))),
            format!("{base}/492.png")
        );
        assert_eq!(
            sprite_url(414, true, false, Some(&f("sandy"))),
            format!("{base}/versions/generation-v/black-white/animated/414.gif")
        );
    }
    // upstream: PokemonFormTests.swift::testCacheKeysSeparateFormsAndKeepDefaultKeys
    #[test]
    fn testCacheKeysSeparateFormsAndKeepDefaultKeys() {
        assert_eq!(cache_key(492, false, false, Some(&f("sky"))), "492-sky-s");
        assert_eq!(cache_key(492, true, true, Some(&f("sky"))), "492-sky-sha");
        assert_eq!(
            cache_key(492, false, false, Some(&f("land"))),
            cache_key(492, false, false, None)
        );
        assert_eq!(
            cache_key(413, false, false, Some(&f("sandy"))),
            "413-sandy-s"
        );
    }
    #[test]
    fn form_roll_keeps_collected_forms_possible() {
        let pool = forms(492);
        let owned = BTreeSet::from([f("land")]);
        assert_eq!(roll(0, &pool, &owned), Some(f("land")));
        assert_eq!(roll(1, &pool, &owned), Some(f("sky")));
        assert_eq!(roll(2, &pool, &owned), Some(f("sky")));
        assert_eq!(roll(3, &pool, &owned), Some(f("land")));
        assert_eq!(roll(0, &[], &owned), None);
    }
    #[test]
    fn form_display_names() {
        assert_eq!(
            display_name("Shaymin", 492, Some(&f("sky")), Some("Forme Céleste")),
            "Shaymin [Forme Céleste]"
        );
        assert_eq!(
            display_name("Shaymin", 492, Some(&f("land")), Some("Forme Terrestre")),
            "Shaymin"
        );
        assert_eq!(display_name("Zarbi", 201, Some(&f("a")), None), "Zarbi [A]");
        assert_eq!(
            display_name("Pikachu", 25, Some(&f("sky")), Some("Sky")),
            "Pikachu"
        );
    }
}
