//! Shared companion labels retain all eight source translations; Shark binds the English column.
use super::format::Language;
#[derive(Clone, Copy, Debug)]
pub enum Label {
    DexRaising,
    UnknownNextEvolution,
    RepresentativePokemonLabel,
    RepresentativeFollowCurrent,
    RepresentativeChooseFromDex,
    RepresentativeSet,
    RepresentativeBadge,
    DexSearchPlaceholder,
    SortTitle,
    FilterShinyOnly,
    NoSearchResults,
    ClearFilters,
    SortDexNumberAsc,
    SortDexNumberDesc,
    SortNameAsc,
    SortNameDesc,
    SortRarity,
    SortDateDesc,
    SortDateAsc,
    TodayTokensShort,
    TodayCost,
    LimitPercent,
    AnimationQualityLabel,
    AnimationQualityHint,
    AnimationPowerSaver,
    AnimationBalanced,
    AnimationSmooth,
}
impl Language {
    pub fn interface_text(self, label: Label) -> &'static str {
        let values = match label {
            Label::DexRaising => [
                "키우는 중",
                "Raising",
                "育成中",
                "Criando",
                "En élevage",
                "Treinando",
                "In Aufzucht",
                "Растёт",
            ],
            Label::UnknownNextEvolution => [
                "알 수 없는 다음 진화",
                "Unknown next evolution",
                "次の進化先は不明",
                "Próxima evolución desconocida",
                "Prochaine évolution inconnue",
                "Próxima evolução desconhecida",
                "Nächste Entwicklung unbekannt",
                "Следующая эволюция неизвестна",
            ],
            Label::RepresentativePokemonLabel => [
                "대표 포켓몬",
                "Representative Pokémon",
                "代表ポケモン",
                "Pokémon representativo",
                "Pokémon représentatif",
                "Pokémon representativo",
                "Repräsentatives Pokémon",
                "Основной покемон",
            ],
            Label::RepresentativeFollowCurrent => [
                "현재 포켓몬 따라가기",
                "Follow current companion",
                "現在のポケモンに合わせる",
                "Seguir al compañero actual",
                "Suivre le compagnon actuel",
                "Seguir o companheiro atual",
                "Aktuellem Begleiter folgen",
                "Текущий компаньон",
            ],
            Label::RepresentativeChooseFromDex => [
                "도감에서 선택…",
                "Choose in Pokédex…",
                "図鑑で選ぶ…",
                "Elegir en la Pokédex…",
                "Choisir dans le Pokédex…",
                "Escolher na Pokédex…",
                "Im Pokédex auswählen…",
                "Выбрать в Покедексе…",
            ],
            Label::RepresentativeSet => [
                "대표로 설정",
                "Set as representative",
                "代表ポケモンに設定",
                "Establecer como representante",
                "Définir comme représentatif",
                "Definir como representante",
                "Als repräsentativ festlegen",
                "Сделать основным",
            ],
            Label::RepresentativeBadge => [
                "대표",
                "Representative",
                "代表",
                "Representante",
                "Représentatif",
                "Representante",
                "Repräsentativ",
                "Основной",
            ],
            Label::DexSearchPlaceholder => [
                "이름 또는 #번호 검색…",
                "Search name or #…",
                "名前または#番号で検索…",
                "Buscar por nombre o #…",
                "Rechercher par nom ou #…",
                "Buscar por nome ou #…",
                "Nach Name oder # suchen…",
                "Поиск по имени или №…",
            ],
            Label::SortTitle => [
                "정렬",
                "Sort",
                "並び替え",
                "Ordenar",
                "Trier",
                "Ordenar",
                "Sortieren",
                "Сортировка",
            ],
            Label::FilterShinyOnly => [
                "이로치만",
                "Shiny only",
                "色違いのみ",
                "Solo variocolor",
                "Chromatique uniquement",
                "Apenas Shiny",
                "Nur schillernde",
                "Только шайни",
            ],
            Label::NoSearchResults => [
                "검색 결과가 없어요",
                "No Pokémon found",
                "見つかりませんでした",
                "No se encontraron Pokémon",
                "Aucun Pokémon trouvé",
                "Nenhum Pokémon encontrado",
                "Keine Pokémon gefunden",
                "Покемоны не найдены",
            ],
            Label::ClearFilters => [
                "필터 초기화",
                "Reset filters",
                "フィルターを解除",
                "Restablecer filtros",
                "Réinitialiser les filtres",
                "Redefinir filtros",
                "Filter zurücksetzen",
                "Сбросить фильтры",
            ],
            Label::SortDexNumberAsc => [
                "도감 번호 (낮은 순)",
                "Number (Low to High)",
                "図鑑番号（昇順）",
                "Nº de Pokédex (asc.)",
                "N° de Pokédex (croissant)",
                "Nº da Pokédex (crescente)",
                "Nummer (aufsteigend)",
                "Номер (по возрастанию)",
            ],
            Label::SortDexNumberDesc => [
                "도감 번호 (높은 순)",
                "Number (High to Low)",
                "図鑑番号（降順）",
                "Nº de Pokédex (desc.)",
                "N° de Pokédex (décroissant)",
                "Nº da Pokédex (decrescente)",
                "Nummer (absteigend)",
                "Номер (по убыванию)",
            ],
            Label::SortNameAsc => [
                "이름 (가나다·A-Z)",
                "Name (A–Z)",
                "名前（五十音・A-Z）",
                "Nombre (A–Z)",
                "Nom (A–Z)",
                "Nome (A–Z)",
                "Name (A–Z)",
                "Имя (A–Z)",
            ],
            Label::SortNameDesc => [
                "이름 (역순)",
                "Name (Z–A)",
                "名前（逆順）",
                "Nombre (Z–A)",
                "Nom (Z–A)",
                "Nome (Z–A)",
                "Name (Z–A)",
                "Имя (Z–A)",
            ],
            Label::SortRarity => [
                "희귀도 순",
                "Rarity (High to Low)",
                "希少度順",
                "Rareza (mayor a menor)",
                "Rareté (décroissante)",
                "Raridade (maior a menor)",
                "Seltenheit (absteigend)",
                "Редкость (по убыванию)",
            ],
            Label::SortDateDesc => [
                "최근 잡은 순",
                "Most Recent",
                "最近捕獲",
                "Más recientes",
                "Plus récents",
                "Mais recentes",
                "Neueste zuerst",
                "Сначала новые",
            ],
            Label::SortDateAsc => [
                "오래된 순",
                "Oldest First",
                "古い順",
                "Más antiguos",
                "Plus anciens",
                "Mais antigos",
                "Älteste zuerst",
                "Сначала старые",
            ],
            Label::TodayTokensShort => [
                "오늘 토큰",
                "Today's tokens",
                "本日のトークン",
                "Tokens de hoy",
                "Tokens du jour",
                "Tokens de hoje",
                "Heutige Tokens",
                "Токены за сегодня",
            ],
            Label::TodayCost => [
                "오늘 비용 ($)",
                "Today's cost ($)",
                "本日のコスト ($)",
                "Coste de hoy ($)",
                "Coût du jour ($)",
                "Custo de hoje ($)",
                "Heutige Kosten ($)",
                "Стоимость за сегодня ($)",
            ],
            Label::LimitPercent => [
                "한도 %",
                "Limit %",
                "上限 %",
                "Límite %",
                "Limite %",
                "Limite %",
                "Limit %",
                "Лимит, %",
            ],
            Label::AnimationQualityLabel => [
                "애니메이션",
                "Animation",
                "アニメーション",
                "Animación",
                "Animation",
                "Animação",
                "Animation",
                "Анимация",
            ],
            Label::AnimationQualityHint => [
                "부드러울수록 배터리를 더 씁니다",
                "Smoother uses more battery",
                "滑らかにするとバッテリー消費が増えます",
                "Más fluido consume más batería",
                "Plus fluide consomme plus de batterie",
                "Mais fluido consome mais bateria",
                "Flüssigere Animationen verbrauchen mehr Batterie",
                "Чем плавнее, тем больше расход батареи",
            ],
            Label::AnimationPowerSaver => [
                "배터리 절약",
                "Power saver",
                "バッテリー優先",
                "Ahorro de batería",
                "Économie d'énergie",
                "Economia de bateria",
                "Energiesparmodus",
                "Экономия энергии",
            ],
            Label::AnimationBalanced => [
                "기본",
                "Balanced",
                "標準",
                "Equilibrado",
                "Équilibré",
                "Equilibrado",
                "Ausgewogen",
                "Баланс",
            ],
            Label::AnimationSmooth => [
                "부드럽게",
                "Smooth",
                "滑らか",
                "Fluido",
                "Fluide",
                "Fluido",
                "Flüssig",
                "Плавность",
            ],
        };
        values[self as usize]
    }
    pub fn dex_raising(self) -> &'static str {
        self.interface_text(Label::DexRaising)
    }
    pub fn unknown_next_evolution(self) -> &'static str {
        self.interface_text(Label::UnknownNextEvolution)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    // upstream: CompanionTests.swift::testDexRaisingLabelLocalized
    // upstream: CompanionTests.swift::testUnknownNextEvolutionAccessibilityLabelLocalized
    #[test]
    fn raising_and_unknown_accessibility_are_localized() {
        assert_eq!(Language::En.dex_raising(), "Raising");
        assert_eq!(Language::Ko.dex_raising(), "키우는 중");
        assert_eq!(Language::Ja.dex_raising(), "育成中");
        assert_eq!(
            Language::Ko.unknown_next_evolution(),
            "알 수 없는 다음 진화"
        );
        assert_eq!(
            Language::En.unknown_next_evolution(),
            "Unknown next evolution"
        );
        assert_eq!(Language::Ja.unknown_next_evolution(), "次の進化先は不明");
    }
    // upstream: PopoverNavigationTests.swift::testGermanSettingsLabelsIncludeLatestMainStrings
    #[test]
    fn german_latest_settings_labels() {
        for (label, want) in [
            (Label::TodayTokensShort, "Heutige Tokens"),
            (Label::TodayCost, "Heutige Kosten ($)"),
            (Label::LimitPercent, "Limit %"),
            (Label::AnimationQualityLabel, "Animation"),
            (
                Label::AnimationQualityHint,
                "Flüssigere Animationen verbrauchen mehr Batterie",
            ),
            (Label::AnimationPowerSaver, "Energiesparmodus"),
            (Label::AnimationBalanced, "Ausgewogen"),
            (Label::AnimationSmooth, "Flüssig"),
        ] {
            assert_eq!(Language::De.interface_text(label), want);
        }
    }
    // upstream: PopoverNavigationTests.swift::testRepresentativeActionsAreLocalizedInEverySupportedLanguage
    #[test]
    fn representative_actions_cover_all_languages() {
        let expected = [
            (
                "대표 포켓몬",
                "현재 포켓몬 따라가기",
                "도감에서 선택…",
                "대표로 설정",
            ),
            (
                "Representative Pokémon",
                "Follow current companion",
                "Choose in Pokédex…",
                "Set as representative",
            ),
            (
                "代表ポケモン",
                "現在のポケモンに合わせる",
                "図鑑で選ぶ…",
                "代表ポケモンに設定",
            ),
            (
                "Pokémon representativo",
                "Seguir al compañero actual",
                "Elegir en la Pokédex…",
                "Establecer como representante",
            ),
            (
                "Pokémon représentatif",
                "Suivre le compagnon actuel",
                "Choisir dans le Pokédex…",
                "Définir comme représentatif",
            ),
            (
                "Pokémon representativo",
                "Seguir o companheiro atual",
                "Escolher na Pokédex…",
                "Definir como representante",
            ),
            (
                "Repräsentatives Pokémon",
                "Aktuellem Begleiter folgen",
                "Im Pokédex auswählen…",
                "Als repräsentativ festlegen",
            ),
            (
                "Основной покемон",
                "Текущий компаньон",
                "Выбрать в Покедексе…",
                "Сделать основным",
            ),
        ];
        for (lang, (label, follow, choose, set)) in Language::ALL.into_iter().zip(expected) {
            assert_eq!(
                lang.interface_text(Label::RepresentativePokemonLabel),
                label
            );
            assert_eq!(
                lang.interface_text(Label::RepresentativeFollowCurrent),
                follow
            );
            assert_eq!(
                lang.interface_text(Label::RepresentativeChooseFromDex),
                choose
            );
            assert_eq!(lang.interface_text(Label::RepresentativeSet), set);
            assert!(!lang.interface_text(Label::RepresentativeBadge).is_empty());
        }
    }
    // upstream: DexSearchAndFilterTests.swift::testSortOptionLabelsExistInAllLanguages
    #[test]
    fn sorting_and_filter_labels_are_complete() {
        for lang in Language::ALL {
            for label in [
                Label::SortDexNumberAsc,
                Label::SortDexNumberDesc,
                Label::SortNameAsc,
                Label::SortNameDesc,
                Label::SortRarity,
                Label::SortDateDesc,
                Label::SortDateAsc,
                Label::DexSearchPlaceholder,
                Label::SortTitle,
                Label::FilterShinyOnly,
                Label::NoSearchResults,
                Label::ClearFilters,
            ] {
                assert!(!lang.interface_text(label).is_empty());
            }
        }
    }
}
