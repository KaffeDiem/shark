//! Display text shared by the companion and its usage surfaces.
pub fn compact(value: u64) -> String {
    if value < 1_000 {
        return value.to_string();
    }
    let (scale, digits, suffix) = if value < 999_950 {
        (1_000., 1, "K")
    } else if value < 999_950_000 {
        (1_000_000., 1, "M")
    } else {
        (1_000_000_000., 2, "B")
    };
    format!("{}{suffix}", trim(value as f64 / scale, digits))
}
pub fn signed_compact(value: i64) -> String {
    format!(
        "{}{}",
        if value < 0 { "-" } else { "" },
        compact(value.unsigned_abs())
    )
}
fn trim(value: f64, digits: usize) -> String {
    format!("{value:.digits$}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}
pub fn percent(value: f64) -> String {
    format!("{}%", trim(value, 1))
}
pub fn difficulty_percent(value: f64) -> String {
    let percent = value * 100.;
    let digits = if percent >= 10. {
        0
    } else if percent >= 1. {
        1
    } else {
        2
    };
    format!("{percent:.digits$}%")
}
pub fn cost(value: f64) -> String {
    format!("${value:.2}")
}
pub fn compact_cost(value: f64) -> String {
    let tenths = format!("{value:.1}");
    if tenths.parse::<f64>().is_ok_and(|n| n < 100.) {
        return format!("${tenths}");
    }
    let whole = format!("{value:.0}");
    if whole.parse::<f64>().is_ok_and(|n| n < 10_000.) {
        format!("${whole}")
    } else {
        format!("${:.1}K", value / 1000.)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Language {
    Ko,
    En,
    Ja,
    Es,
    Fr,
    Pt,
    De,
    Ru,
}
impl Language {
    pub const ALL: [Self; 8] = [
        Self::Ko,
        Self::En,
        Self::Ja,
        Self::Es,
        Self::Fr,
        Self::Pt,
        Self::De,
        Self::Ru,
    ];
    fn pick(self, values: [String; 8]) -> String {
        values.into_iter().nth(self as usize).unwrap()
    }
    pub fn egg_hint(self, amount: &str) -> String {
        self.pick([
  format!("로컬 AI 코딩 도구의 사용량으로 자라요. 약 {amount} 토큰을 쓰면 알이 부화해요."),
  format!("Grows from your local AI coding usage. Your egg hatches after ~{amount} tokens."),
  format!("ローカルの AI コーディング使用量で育ちます。約{amount}トークンでタマゴが孵化します。"),
  format!("Crece con el uso de tus herramientas locales de programación con IA. Tu huevo eclosiona tras unos {amount} de tokens."),
  format!("Il grandit avec l'usage de tes outils de code IA locaux. Ton œuf éclôt après environ {amount} de tokens."),
  format!("Cresce com o uso das suas ferramentas locais de programação com IA. O ovo choca depois de uns {amount} de tokens."),
  format!("Wächst mit der Nutzung deiner lokalen KI-Coding-Tools. Nach etwa {amount} Tokens schlüpft dein Ei."),
  format!("Растёт вместе с вашим локальным использованием ИИ для программирования. Яйцо вылупится примерно через {amount} токенов.")])
    }
    pub fn shiny_hatch(self, name: &str, odds: u64) -> String {
        let subject = KoreanParticle::Subject.attach(name);
        self.pick([
            format!("이로치 {subject} 태어났어요! (1/{odds})"),
            format!("A shiny {name} hatched! (1 in {odds})"),
            format!("色違いの {name} が生まれました！(1/{odds})"),
            format!("¡Nació un {name} variocolor! (1 entre {odds})"),
            format!("Un {name} chromatique est né ! (1 sur {odds})"),
            format!("Nasceu um {name} shiny! (1 em {odds})"),
            format!("Ein schillerndes {name} ist geschlüpft! (1/{odds})"),
            format!("Вылупился шайни {name}! (шанс 1 к {odds})"),
        ])
    }
    pub fn hatch(self, name: &str) -> String {
        self.pick([
            format!("알에서 {} 나왔어요!", KoreanParticle::Subject.attach(name)),
            format!("{name} hatched from the egg!"),
            format!("タマゴから {name} が生まれました！"),
            format!("¡{name} salió del huevo!"),
            format!("{name} a éclos de l’œuf !"),
            format!("{name} nasceu do ovo!"),
            format!("{name} ist aus dem Ei geschlüpft!"),
            format!("Из яйца вылупился {name}!"),
        ])
    }
    pub fn evolved(self, name: &str) -> String {
        self.pick([
            format!("{} 진화했어요!", KoreanParticle::Direction.attach(name)),
            format!("Evolved into {name}!"),
            format!("{name} に進化しました！"),
            format!("¡Evolucionó a {name}!"),
            format!("A évolué en {name} !"),
            format!("Evoluiu para {name}!"),
            format!("Hat sich zu {name} entwickelt!"),
            format!("Эволюционировал в {name}!"),
        ])
    }
    pub fn egg_confirm(self, name: &str, egg: &str) -> String {
        self.pick([
            format!(
                "{} 놓아주고 {} 바꿀까요?",
                KoreanParticle::Object.attach(name),
                KoreanParticle::Direction.attach(egg)
            ),
            format!("Release {name} and start a {egg}?"),
            format!("{name} を逃がして {egg} に変えますか？"),
            format!("¿Liberar a {name} y empezar con {egg}?"),
            format!("Relâcher {name} et commencer avec {egg} ?"),
            format!("Soltar {name} e começar com {egg}?"),
            format!("{name} freilassen und mit {egg} beginnen?"),
            format!("Отпустить {name} и начать с {egg}?"),
        ])
    }

    pub fn shiny_ditto(self, name: &str) -> String {
        self.pick([
            format!("{name}인 줄 알았는데 — 이로치 메타몽이었어요!"),
            format!("You thought it was {name} — it was a shiny Ditto!"),
            format!("{name} だと思ってた… 色違いのメタモンでした！"),
            format!("Pensabas que era {name} — ¡era un Ditto variocolor!"),
            format!("Tu croyais que c'était {name} — c'était un Métamorph chromatique !"),
            format!("Você achava que era {name} — era um Ditto shiny!"),
            format!("Du dachtest, es wäre {name} – dabei war es ein schillerndes Ditto!"),
            format!("Думали, это {name}? А это шайни-Ditto!"),
        ])
    }
    pub fn candy_title(self, item: &str, count: u64) -> String {
        self.pick([
            format!("🍬 {item} {count}개를 받았어요!"),
            format!("🍬 You got {count}× {item}!"),
            format!("🍬 {item}を{count}個もらいました！"),
            format!("🍬 ¡Has recibido {count}× {item}!"),
            format!("🍬 Tu as reçu {count}× {item} !"),
            format!("🍬 Você ganhou {count}× {item}!"),
            format!("🍬 Du hast {count}× {item} erhalten!"),
            format!("🍬 Вы получили: {item} ×{count}!"),
        ])
    }
    pub fn candy_body(self, window: &str) -> String {
        self.pick([format!("{window} 토큰 한도를 다 채웠어요. 열심히 쓴 만큼 사탕을 드려요 — 포켓몬에게 써서 진화시켜 보세요!"),format!("You maxed out your {window} token limit. A treat for the effort — use it to evolve your Pokémon!"),format!("{window}のトークン上限を使い切りました。がんばったごほうびです — ポケモンに使って進化させよう！"),format!("Has agotado tu límite de tokens {window}. Un premio por el esfuerzo — ¡úsalo para evolucionar a tu Pokémon!"),format!("Tu as atteint ta limite de tokens {window}. Une récompense pour l'effort — utilise-la pour faire évoluer ton Pokémon !"),format!("Você esgotou seu limite de tokens — {window}. Você merece um agrado: use no seu Pokémon para evoluir!"),format!("Du hast das Token-Limit für {window} ausgeschöpft. Eine Belohnung für deinen Einsatz – verwende sie, um dein Pokémon zu entwickeln!"),format!("Вы полностью исчерпали лимит токенов ({window}). Это награда за старания — используйте её, чтобы эволюционировать покемона!")])
    }
}

/// Locale-selected grouping separator; grouping never depends on the machine running tests.
pub fn grouped(value: u64, separator: char) -> String {
    let digits = value.to_string();
    let mut result = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            result.push(separator)
        }
        result.push(c)
    }
    result
}

#[derive(Clone, Copy)]
pub enum KoreanParticle {
    Subject,
    Object,
    Direction,
}
impl KoreanParticle {
    pub fn attach(self, name: &str) -> String {
        let final_consonant = name
            .chars()
            .last()
            .filter(|c| ('\u{ac00}'..='\u{d7a3}').contains(c))
            .map(|c| (c as u32 - 0xac00) % 28);
        let suffix = match (self, final_consonant) {
            (Self::Subject, Some(0)) => "가",
            (Self::Subject, Some(_)) => "이",
            (Self::Subject, None) => "이(가)",
            (Self::Object, Some(0)) => "를",
            (Self::Object, Some(_)) => "을",
            (Self::Object, None) => "을(를)",
            (Self::Direction, Some(0 | 8)) => "로",
            (Self::Direction, Some(_)) => "으로",
            (Self::Direction, None) => "(으)로",
        };
        format!("{name}{suffix}")
    }
}

pub fn progress_detail(used: u64, total: u64) -> String {
    let used = used.min(total);
    let percent = if total == 0 {
        0
    } else {
        (used as u128 * 100 / total as u128) as u64
    };
    format!("{} / {} · {percent}%", compact(used), compact(total))
}

#[cfg(test)]
#[path = "tests/format_extra.rs"]
mod tests;
