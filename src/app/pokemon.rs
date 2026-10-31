//! PokeTokenBar's companion lives in the navigation footer; account usage shares its popover.
use super::*;
mod catch_log;
mod detail_appearance;
mod dex;
mod evolution_line;
mod footer_animation;
mod footer_usage;
use crate::ui::pokemon_images as images;
mod activity;
#[cfg(all(test, feature = "test-support"))]
mod activity_tests;
mod names;
mod presentation;
mod recap_assets;
mod sprite_loading;
use crate::pokemon::names_client::{
    Item as NameItem, Kind as NameKind, Names as ResourceNames, Resource as NameResource,
};
use crate::pokemon::{Balance, Companion, Event, Item, Mon, PokeApi, Rarity, profile};
use crate::usage::providers::ProviderHandle;
use gpui_kit::component::input::InputEvent;
use gpui_kit::component::{Disableable as _, Selectable as _};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Page {
    Home,
    Dex,
    Details,
    Bag,
    Shop,
    #[default]
    Usage,
}

#[derive(Default)]
pub(super) struct CompanionUi {
    pub tab: Page,
    activity_roster: Option<std::sync::Arc<Vec<crate::chat::activity::Pokemon>>>,
    pub scanning: Vec<bool>,
    pub generation: u64,
    hatch_pending: bool,
    celebration: presentation::Celebration,
    celebration_timer: Option<f64>,
    power_task: Option<Task<()>>,
    low_power: bool,
    window_active: bool,
    footer_task: Option<Task<()>>,
    footer_key: Option<footer_animation::Key>,
    footer_render: Option<std::sync::Arc<RenderImage>>,
    footer_asset: Option<std::sync::Arc<crate::pokemon::sprites::Asset>>,
    footer_frame: usize,
    hatch_retry_delayed: bool,
    name_pending: BTreeSet<u16>,
    name_failed: BTreeSet<u16>,
    resource_names: BTreeMap<NameResource, ResourceNames>,
    resource_wanted: BTreeSet<NameResource>,
    resource_pending: BTreeMap<NameResource, u64>,
    resource_checked: BTreeSet<NameResource>,
    resource_failed: BTreeSet<NameResource>,
    resource_generation: u64,
    sprite_pending: bool,
    sprite_task: Option<Task<()>>,
    sprite_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    sprite_request: u64,
    sprite_retry_task: Option<Task<()>>,
    sprite_retry_count: u8,
    sprite_key: Option<(u16, bool, Option<crate::pokemon::forms::Form>)>,
    sprite: Option<PathBuf>,
    notice: Option<String>,
    search: Option<Entity<InputState>>,
    search_sub: Option<Subscription>,
    dex_page: usize,
    rarity: Option<Rarity>,
    shiny_only: bool,
    sort: dex::Sort,
    catch_sort: Option<dex::Sort>,
    catch_log: bool,
    pager: dex::ScrollPager,
    catch_scroll: UniformListScrollHandle,
    catch_visible: BTreeSet<SpriteIdentity>,
    catch_layout_count: Rc<std::cell::Cell<usize>>,
    evolution_home_scroll: ScrollHandle,
    evolution_detail_scroll: ScrollHandle,
    pub(super) selected_species: Option<u16>,
    individual: usize,
    detail_form: Option<crate::pokemon::forms::Form>,
    details: BTreeMap<u16, profile::Details>,
    detail_pending: HashSet<u16>,
    detail_deferred: Option<u16>,
    thumbnails: BTreeMap<SpriteIdentity, PathBuf>,
    thumbnail_pending: BTreeSet<SpriteIdentity>,
    thumbnail_failed: BTreeSet<SpriteIdentity>,
    pub(super) growth_draft: Option<f64>,
    pub(super) shop_draft: Option<f64>,
    quantity: u64,
}

use crate::pokemon::format::compact;

type SpriteIdentity = (u16, bool, Option<crate::pokemon::forms::Form>);
fn sprite_identity(companion: &Companion) -> Option<SpriteIdentity> {
    companion
        .representative_subject()
        .map(|(id, shiny)| (id, shiny, companion.representative_sprite_form()))
}
fn mon_sprite_identity(mon: &Mon, species: u16) -> SpriteIdentity {
    (
        species,
        mon.displays_shiny(),
        crate::pokemon::forms::resolved(species, mon.form.as_ref()),
    )
}
fn sprite_needs_reload(
    current: &Option<SpriteIdentity>,
    loaded: bool,
    desired: &Option<SpriteIdentity>,
) -> bool {
    current != desired || (!loaded && desired.is_some())
}

fn api() -> PokeApi {
    PokeApi::new(crate::state::support_dir().join("pokemon-cache"))
}

impl Shark {
    #[cfg(test)]
    pub(super) fn seed_pokemon_preview_sprites(
        &mut self,
        directory: &Path,
        cx: &mut Context<Self>,
    ) {
        for mon in self
            .state
            .pokemon
            .collection
            .iter()
            .chain(self.state.pokemon.active.iter())
        {
            for species in mon.reached_species() {
                let key = mon_sprite_identity(mon, species);
                let exact = directory.join(
                    crate::pokemon::sprites::Request::pokemon(species, true, key.1, key.2.as_ref())
                        .filename(),
                );
                let variant = directory.join(format!(
                    "{species}{}.gif",
                    if key.1 { "-shiny" } else { "" }
                ));
                let path = if exact.is_file() {
                    exact
                } else if variant.is_file() {
                    variant
                } else {
                    directory.join(format!("{species}.gif"))
                };
                if path.is_file() {
                    let path = crate::pokemon::sprites::cached(&path)
                        .unwrap_or_else(|| {
                            panic!(
                                "Runtime preview GIF must pass production validation: {}",
                                path.display()
                            )
                        })
                        .path
                        .clone();
                    self.pokemon_ui
                        .thumbnails
                        .insert(mon_sprite_identity(mon, species), path);
                }
            }
        }
        self.pokemon_ui.sprite_key = sprite_identity(&self.state.pokemon);
        self.pokemon_ui.sprite = self
            .pokemon_ui
            .sprite_key
            .as_ref()
            .and_then(|key| self.pokemon_ui.thumbnails.get(key).cloned());
        self.refresh_pokemon_animation(cx);
    }

    pub(crate) fn apply_pokemon_save(&mut self, mut companion: Companion, cx: &mut Context<Self>) {
        companion.sanitize();
        self.state.pokemon = companion;
        let generation = self.pokemon_ui.generation.wrapping_add(1);
        let scanning = std::mem::take(&mut self.pokemon_ui.scanning);
        self.pokemon_ui = CompanionUi {
            generation,
            scanning,
            ..Default::default()
        };
        self.save_pokemon();
        self.load_pokemon(cx);
        self.refresh_usage(cx);
        cx.notify();
    }

    pub(crate) fn set_pokemon_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.state.pokemon_disabled == !enabled {
            return;
        }
        self.state.pokemon_disabled = !enabled;
        self.invalidate_pokemon_requests();
        // The first completed scan after a toggle is a baseline, so disabled intervals never earn growth.
        self.state.pokemon.baselines.clear();
        self.save_pokemon();
        self.refresh_usage(cx);
        if enabled {
            self.load_pokemon(cx);
        }
        cx.notify();
    }

    pub(crate) fn set_usage_roots(&mut self, provider_id: &str, raw: &str, cx: &mut Context<Self>) {
        let Some(provider) = self.usage_providers.iter().copied().find(|provider| {
            provider.id() == provider_id && provider.source().supports_custom_roots()
        }) else {
            return;
        };
        let raw = raw.trim();
        if self
            .state
            .usage_roots
            .get(provider_id)
            .map(String::as_str)
            .unwrap_or("")
            == raw
        {
            return;
        }
        if raw.is_empty() {
            self.state.usage_roots.remove(provider_id);
        } else {
            self.state
                .usage_roots
                .insert(provider_id.to_owned(), raw.to_owned());
        }
        self.invalidate_pokemon_requests();
        // Newly included roots can contain years of history. Their first scan is a
        // baseline, so choosing a folder never mints retroactive companion currency.
        self.state.pokemon.baselines.remove(provider_id);
        provider.source().configure_scan_roots(raw);
        self.save_pokemon();
        self.refresh_usage(cx);
        self.load_pokemon(cx);
        cx.notify();
    }

    fn invalidate_pokemon_requests(&mut self) {
        self.pokemon_ui.generation = self.pokemon_ui.generation.wrapping_add(1);
        self.pokemon_ui.hatch_pending = false;
        self.pokemon_ui.hatch_retry_delayed = false;
        self.pokemon_ui.name_pending.clear();
        self.pokemon_ui.name_failed.clear();
        self.pokemon_ui.resource_generation = self.pokemon_ui.resource_generation.wrapping_add(1);
        self.pokemon_ui.resource_pending.clear();
        self.pokemon_ui.resource_wanted.clear();
        self.pokemon_ui.resource_checked.clear();
        self.pokemon_ui.resource_failed.clear();
        self.pokemon_ui.celebration = Default::default();
        self.pokemon_ui.celebration_timer = None;
        self.pokemon_ui.power_task.take();
        self.pokemon_ui.footer_task.take();
        self.pokemon_ui.footer_key = None;
        self.pokemon_ui.footer_render = None;
        self.pokemon_ui.footer_asset = None;
        self.pokemon_ui.footer_frame = 0;
        self.cancel_pokemon_sprite();
        self.pokemon_ui.sprite_retry_task.take();
        self.pokemon_ui.sprite_retry_count = 0;
        self.pokemon_ui.detail_pending.clear();
        self.pokemon_ui.detail_deferred = None;
        self.pokemon_ui.thumbnail_pending.clear();
        self.pokemon_ui.thumbnail_failed.clear();
    }

    fn review_pokemon_egg(
        &mut self,
        tier: Option<Rarity>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.state.pokemon.is_high_value_companion() {
            self.start_pokemon_egg(tier, cx);
            return;
        }
        let this = cx.entity().downgrade();
        let generation = self.pokemon_ui.generation;
        let individual = self
            .state
            .pokemon
            .active
            .as_ref()
            .and_then(|m| m.profile.as_ref())
            .map(|p| p.instance_id.clone());
        let summary = crate::pokemon::format::Language::En
            .egg_confirm(&self.state.pokemon.title(), "a fresh egg");
        let warning = crate::pokemon::format::Language::En.valuable_warning(
            self.state
                .pokemon
                .active
                .as_ref()
                .is_some_and(Mon::displays_shiny),
        );
        window.open_dialog(cx, move |d, _, _| {
            let this = this.clone();
            let individual = individual.clone();
            crate::ui::dialog(d)
                .title("Start a fresh egg?")
                .w(px(480.))
                .on_ok(|_, _, _| true)
                .child(summary.clone())
                .child(warning)
                .footer(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("pokemon-egg-cancel")
                                .label("Keep growing")
                                .on_click(|_, w, cx| w.close_dialog(cx)),
                        )
                        .child(
                            Button::new("pokemon-egg-confirm")
                                .danger()
                                .label(format!(
                                    "{} and start egg",
                                    crate::pokemon::format::Language::En.send_off()
                                ))
                                .on_click(move |_, w, cx| {
                                    _ = this.update(cx, |s, cx| {
                                        let current = s
                                            .state
                                            .pokemon
                                            .active
                                            .as_ref()
                                            .and_then(|m| m.profile.as_ref())
                                            .map(|p| p.instance_id.clone());
                                        if !s.state.pokemon_disabled
                                            && s.pokemon_ui.generation == generation
                                            && current == individual
                                        {
                                            s.start_pokemon_egg(tier, cx);
                                        }
                                    });
                                    w.close_dialog(cx);
                                }),
                        ),
                )
        });
    }

    fn start_pokemon_egg(&mut self, tier: Option<Rarity>, cx: &mut Context<Self>) {
        self.pokemon_ui.notice = Some(match self.state.pokemon.fresh_egg(tier) {
            Ok(()) => {
                self.invalidate_pokemon_requests();
                "A fresh egg is ready to raise.".into()
            }
            Err(error) => error,
        });
        self.save_pokemon();
        self.load_pokemon(cx);
        cx.notify();
    }

    pub(super) fn observe_pokemon_limits(&mut self, provider: impl Into<ProviderHandle>) {
        let provider = provider.into();
        let windows = provider
            .source()
            .candy_windows(&self.usage[provider.index()]);
        let mut notices = vec![];
        for window in windows {
            let count = self.state.pokemon.observe_limit(
                &window.key,
                window.epoch.as_deref(),
                window.utilization,
                window.weekly,
                !self.state.pokemon_disabled,
            );
            if count > 0 {
                notices.push(format!(
                    "{} reached — received {count} Rare Candy!",
                    window.name
                ));
            }
        }
        if !notices.is_empty() {
            self.pokemon_ui.notice = Some(notices.join(" · "));
        }
        self.save_pokemon();
    }

    #[cfg(test)]
    pub(super) fn observe_pokemon_usage(
        &mut self,
        provider: impl Into<ProviderHandle>,
        cx: &mut Context<Self>,
    ) {
        let provider = provider.into();
        let daily = self.usage_activity[provider.index()].clone();
        self.usage_reports[provider.index()] =
            crate::usage::logs::Report::from_daily(daily.clone());
        self.observe_pokemon_usage_daily(provider, &daily, cx);
    }

    pub(super) fn observe_pokemon_usage_daily(
        &mut self,
        provider: ProviderHandle,
        daily: &crate::usage::activity::Daily,
        cx: &mut Context<Self>,
    ) {
        let Some(&total) = daily.tokens.last() else {
            return;
        };
        let day = daily.first_day + daily.tokens.len() as i64 - 1;
        let events = self.state.pokemon.observe_usage(
            provider.source().id(),
            &format!("{day:010}"),
            total,
            !self.state.pokemon_disabled,
        );
        self.pokemon_events(events);
        self.save_pokemon();
        self.load_pokemon(cx);
    }

    fn pokemon_events(&mut self, events: Vec<Event>) {
        if !events.is_empty() {
            let lang = crate::pokemon::format::Language::En;
            let notice = events
                .iter()
                .map(|event| match event {
                    Event::Hatched(n) => lang.hatch(n),
                    Event::Evolved(n) => lang.evolved(n),
                    _ => event.to_string(),
                })
                .collect::<Vec<_>>()
                .join(" · ");
            self.pokemon_ui.notice = Some(notice.clone());
            self.pokemon_ui
                .celebration
                .observe(&events, &self.state.pokemon, crate::usage::now());
            self.pokemon_ui.celebration.notice = Some(notice);
        }
    }

    fn schedule_pokemon_celebration(&mut self, cx: &mut Context<Self>) {
        let until = self.pokemon_ui.celebration.until;
        if self.pokemon_ui.celebration.message.is_none()
            || self.pokemon_ui.celebration_timer == Some(until)
        {
            return;
        }
        self.pokemon_ui.celebration_timer = Some(until);
        let generation = self.pokemon_ui.generation;
        let delay = Duration::from_secs_f64((until - crate::usage::now()).max(0.));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            _ = this.update(cx, |s, cx| {
                if s.pokemon_ui.generation == generation && s.pokemon_ui.celebration.until == until
                {
                    if s.pokemon_ui.notice == s.pokemon_ui.celebration.notice {
                        s.pokemon_ui.notice = None;
                    }
                    s.pokemon_ui.celebration = Default::default();
                    s.pokemon_ui.celebration_timer = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn ensure_pokemon_power_observation(&mut self, cx: &mut Context<Self>) {
        if self.pokemon_ui.power_task.is_some() {
            return;
        }
        self.pokemon_ui.low_power =
            objc2_foundation::NSProcessInfo::processInfo().isLowPowerModeEnabled();
        let generation = self.pokemon_ui.generation;
        self.pokemon_ui.power_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(5)).await;
                let keep = this
                    .update(cx, |s, cx| {
                        if s.state.pokemon_disabled || s.pokemon_ui.generation != generation {
                            return false;
                        }
                        let power =
                            objc2_foundation::NSProcessInfo::processInfo().isLowPowerModeEnabled();
                        if power != s.pokemon_ui.low_power {
                            s.pokemon_ui.low_power = power;
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        }));
    }

    fn pokemon_mood(&self) -> presentation::Mood {
        let now = crate::usage::now();
        let today = self
            .usage_reports
            .iter()
            .map(|r| r.today())
            .fold(0, u64::saturating_add);
        let has_usage = self.usage_activity.iter().any(|d| !d.tokens.is_empty());
        let burn = self
            .usage_reports
            .iter()
            .filter_map(|r| r.active_block(now))
            .map(|b| b.tokens_per_minute)
            .sum();
        let warning = self.observed_headline_warning();
        presentation::mood(
            self.state.pokemon.active.is_some(),
            &self.pokemon_ui.celebration,
            now,
            has_usage,
            today,
            burn,
            warning,
        )
    }

    fn save_pokemon(&mut self) {
        self.pokemon_ui.activity_roster = None;
        if let Err(error) = self.state.save() {
            self.pokemon_ui.notice = Some(format!("Could not save Pokémon progress: {error}"));
        }
    }

    pub(super) fn load_pokemon(&mut self, cx: &mut Context<Self>) {
        self.pokemon_ui.activity_roster = None;
        if self.state.pokemon_disabled {
            return;
        }
        if self.state.pokemon.ready_to_hatch() && self.state.pokemon.pending_line.is_some() {
            let events = self.state.pokemon.hatch_prepared();
            self.pokemon_events(events);
            self.save_pokemon();
        }
        self.ensure_pokemon_power_observation(cx);
        self.schedule_pokemon_celebration(cx);
        self.load_active_pokemon_profile(cx);
        if let Some(key) = self
            .state
            .pokemon
            .active
            .as_ref()
            .map(|mon| mon_sprite_identity(mon, mon.species_id()))
        {
            self.load_pokemon_thumbnail(key, cx);
        }
        self.load_pokemon_sprite(cx);
        // Headless tests provide fixtures and never contact third-party services.
        #[cfg(test)]
        if !self.usage_live {
            return;
        }
        let reveal = self.state.pokemon.ready_to_reveal();
        let needs_line = self.state.pokemon.active.is_none()
            && self.state.pokemon.pending_line.is_none()
            && self.state.pokemon.egg_tokens > 0;
        if (needs_line || reveal) && !self.pokemon_ui.hatch_pending {
            self.pokemon_ui.hatch_pending = true;
            let generation = self.pokemon_ui.generation;
            let individual = self
                .state
                .pokemon
                .active
                .as_ref()
                .and_then(|m| m.profile.as_ref())
                .map(|p| p.instance_id.clone());
            let rarity = self.state.pokemon.min_rarity();
            let collected = self
                .state
                .pokemon
                .collection
                .iter()
                .filter(|m| m.graduated)
                .map(|m| m.line.base_id)
                .collect();
            cx.spawn(async move |this, cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        if reveal {
                            api().line(132)
                        } else {
                            api().random_line_with_collection(rarity, &collected)
                        }
                    })
                    .await;
                this.update(cx, |s, cx| {
                    s.complete_pokemon_hatch(generation, individual.as_deref(), reveal, result, cx)
                })
                .ok();
            })
            .detach();
        }
        self.load_active_pokemon_profile(cx);
    }

    fn complete_pokemon_hatch(
        &mut self,
        generation: u64,
        individual: Option<&str>,
        reveal: bool,
        result: Result<crate::pokemon::EvoLine, String>,
        cx: &mut Context<Self>,
    ) {
        if self.pokemon_ui.generation != generation {
            return;
        }
        self.pokemon_ui.hatch_pending = false;
        let current = self
            .state
            .pokemon
            .active
            .as_ref()
            .and_then(|m| m.profile.as_ref())
            .map(|p| p.instance_id.as_str());
        if current != individual {
            self.load_pokemon(cx);
            return;
        }
        if !self.state.pokemon_disabled {
            match result {
                Ok(line) => {
                    self.pokemon_ui.hatch_retry_delayed = false;
                    self.pokemon_ui.notice = None;
                    let events = if reveal && self.state.pokemon.ready_to_reveal() {
                        self.state.pokemon.reveal_ditto(line)
                    } else if !reveal && self.state.pokemon.active.is_none() {
                        self.state.pokemon.prepare_egg(line);
                        if self.state.pokemon.ready_to_hatch() {
                            self.state.pokemon.hatch_prepared()
                        } else {
                            vec![]
                        }
                    } else {
                        vec![]
                    };
                    self.pokemon_events(events);
                    self.save_pokemon();
                }
                Err(error) => {
                    self.pokemon_ui.hatch_retry_delayed = self.state.pokemon.ready_to_hatch();
                    self.pokemon_ui.notice = Some(format!(
                        "Could not prepare Pokémon. Refresh to retry: {error}"
                    ))
                }
            }
        }
        // The line request releases its lock before optional details are loaded.
        self.load_active_pokemon_profile(cx);
        #[cfg(not(test))]
        self.load_pokemon_sprite(cx);
        #[cfg(test)]
        if self.usage_live {
            self.load_pokemon_sprite(cx);
        }
        self.schedule_pokemon_celebration(cx);
        cx.notify();
    }

    fn load_active_pokemon_profile(&mut self, cx: &mut Context<Self>) {
        if let Some(species) = self.state.pokemon.active.as_ref().map(|m| m.species_id()) {
            self.request_pokemon_details(species, cx);
        }
    }

    pub(super) fn show_pokemon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_companion_panel(window, cx);
        if self.pokemon_ui.search.is_none() {
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder(
                    crate::pokemon::format::Language::En.interface_text(
                        crate::pokemon::interface_labels::Label::DexSearchPlaceholder,
                    ),
                )
            });
            self.pokemon_ui.search_sub = Some(cx.subscribe(&input, |s, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    s.pokemon_ui.dex_page = 0;
                    s.load_dex_assets(cx);
                    cx.notify();
                }
            }));
            self.pokemon_ui.search = Some(input);
        }
        self.pokemon_ui.tab = Page::Home;
        self.pokemon_ui
            .evolution_home_scroll
            .set_offset(point(px(0.), px(0.)));
        self.pokemon_ui.selected_species = None;
        self.sync_profile_names(false, cx);
        self.load_pokemon(cx);
        cx.notify();
    }

    pub(crate) fn show_pokemon_dex(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_pokemon(window, cx);
        self.pokemon_ui.tab = Page::Dex;
        self.pokemon_ui.catch_log = false;
        self.load_dex_assets(cx);
        cx.notify();
    }

    fn pokemon_art(&self, size: f32) -> AnyElement {
        if let (Some(path), Some(image), Some(key)) = (
            &self.pokemon_ui.sprite,
            &self.pokemon_ui.footer_render,
            &self.pokemon_ui.footer_key,
        ) {
            if &key.path == path {
                return images::paint_frame(image.clone(), self.pokemon_ui.footer_frame, size);
            }
        }
        if let Some(image) = self.pokemon_ui.sprite.as_ref().and_then(|path| {
            images::cached_frame(
                path,
                self.state
                    .pokemon_animation_quality
                    .effective_floor(self.pokemon_ui.low_power),
            )
        }) {
            // A cold decoded-asset cache may require background disk work. Until it finishes,
            // paint a cached first frame without starting native Img's uncapped animation loop.
            images::paint_frame(image, 0, size)
        } else {
            div()
                .size(px(size))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(size * 0.6))
                .child(if self.state.pokemon.active.is_none() {
                    "🥚"
                } else {
                    "✦"
                })
                .into_any_element()
        }
    }

    pub(super) fn render_pokemon_footer(&self, cx: &Context<Self>) -> AnyElement {
        let now = crate::usage::now();
        let quotas: Vec<_> = self
            .usage_providers
            .iter()
            .map(|provider| {
                (
                    provider.id(),
                    footer_usage::quota_lines(
                        provider.name(),
                        &self.usage[provider.index()],
                        now,
                        self.rail,
                    ),
                )
            })
            .collect();
        let quota = self
            .usage_providers
            .iter()
            .flat_map(|provider| {
                footer_usage::quota_details(provider.name(), &self.usage[provider.index()], now)
            })
            .collect::<Vec<_>>()
            .join(" · ");
        Button::new("pokemon-footer")
            .ghost()
            .h_auto()
            .p_1()
            .w_full()
            .accessibility_label(format!("{quota}. Open Pokémon companion"))
            .tooltip(quota)
            .on_click(cx.listener(|s, _, w, cx| s.show_pokemon(w, cx)))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .w_full()
                    .items_center()
                    .when(self.rail, |d| d.flex_col())
                    .child(self.pokemon_art(40.))
                    .child(
                        v_flex()
                            .min_w_0()
                            .flex_1()
                            .when(self.rail, |d| d.w_full().items_center())
                            .when(!self.rail, |d| d.items_start())
                            .gap_1()
                            .children(quotas.into_iter().map(|(id, lines)| {
                                v_flex()
                                    .id(format!("pokemon-footer-quota-{id}"))
                                    .text_xs()
                                    .when(self.rail, |d| d.text_size(px(10.)).items_center())
                                    .children(lines.into_iter().map(|line| div().child(line)))
                                    .test_support()
                            })),
                    ),
            )
            .into_any_element()
    }

    fn active_pokemon_art(&self, size: f32) -> AnyElement {
        let Some(mon) = self.state.pokemon.active.as_ref() else {
            return div()
                .size(px(size))
                .text_size(px(size * 0.6))
                .flex()
                .items_center()
                .justify_center()
                .child("🥚")
                .into_any_element();
        };
        let key = mon_sprite_identity(mon, mon.species_id());
        let path = self.pokemon_ui.thumbnails.get(&key).or_else(|| {
            (self.pokemon_ui.sprite_key.as_ref() == Some(&key))
                .then_some(self.pokemon_ui.sprite.as_ref())
                .flatten()
        });
        if let Some(path) = path {
            images::sized(path, size)
                .id("pokemon-active-sprite")
                .object_fit(ObjectFit::Contain)
                .into_any_element()
        } else {
            div()
                .size(px(size))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(size * 0.6))
                .child("✦")
                .into_any_element()
        }
    }

    pub(super) fn render_pokemon_panel(&self, cx: &Context<Self>) -> AnyElement {
        let page = self.pokemon_ui.tab;
        let body = match page {
            Page::Dex => self.pokemon_dex(cx),
            Page::Details => self.pokemon_details(cx),
            Page::Bag => self.pokemon_bag(cx),
            Page::Shop => self.pokemon_shop(cx),
            _ => self.pokemon_home(cx),
        };
        v_flex()
            .id("pokemon-panel")
            .gap_3()
            .child(
                h_flex()
                    .justify_between()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Pokémon"))
                    .child(
                        Button::new("pokemon-refresh")
                            .ghost()
                            .xsmall()
                            .icon(IconName::RefreshCw)
                            .accessibility_label("Refresh Pokémon and usage")
                            .on_click(cx.listener(|s, _, _, cx| {
                                s.refresh_pokemon_artwork(cx);
                                s.refresh_usage(cx);
                                s.load_pokemon(cx);
                            })),
                    ),
            )
            .child(
                h_flex().gap_1().children(
                    [
                        (Page::Home, "Home"),
                        (Page::Dex, "Pokédex"),
                        (Page::Bag, "Bag"),
                        (Page::Shop, "Shop"),
                        (Page::Usage, "Usage"),
                    ]
                    .into_iter()
                    .map(|(tab, label)| {
                        Button::new(format!("pokemon-tab-{label}"))
                            .ghost()
                            .xsmall()
                            .label(label)
                            .selected(page == tab || (page == Page::Details && tab == Page::Dex))
                            .toggled(page == tab || (page == Page::Details && tab == Page::Dex))
                            .on_click(cx.listener(move |s, _, _, cx| {
                                if s.pokemon_ui.tab == Page::Details && tab == Page::Dex {
                                    return;
                                }
                                s.pokemon_ui.tab = tab;
                                s.pokemon_ui.selected_species = None;
                                s.sync_profile_names(false, cx);
                                if tab == Page::Dex {
                                    s.load_dex_assets(cx);
                                }
                                cx.notify();
                            }))
                    }),
                ),
            )
            .children(self.pokemon_ui.notice.as_ref().map(|text| {
                div()
                    .id("pokemon-notice")
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(text.clone())
                    .test_support()
            }))
            .when(page != Page::Usage, |d| d.child(body))
            .test_support()
            .into_any_element()
    }

    fn pokemon_home(&self, cx: &Context<Self>) -> AnyElement {
        let p = &self.state.pokemon;
        let (used, target) = p.progress();
        let progress = (used as f64 / target.max(1) as f64).clamp(0., 1.);
        v_flex()
            .id("pokemon-home")
            .items_center()
            .gap_2()
            .child(if let Some(mon) = p.active.as_ref() {
                let species = mon.species_id();
                Button::new("pokemon-home-open-details")
                    .ghost()
                    .h_auto()
                    .p_0()
                    .accessibility_label(format!("Open {} details", mon.line.name(species)))
                    .on_click(cx.listener(move |s, _, _, cx| s.select_pokemon(species, cx)))
                    .child(self.active_pokemon_art(96.))
                    .into_any_element()
            } else {
                self.active_pokemon_art(96.)
            })
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(p.title()),
            )
            .children(p.active.as_ref().map(|mon| {
                div().text_xs().child(format!(
                    "{} nature{}",
                    crate::pokemon::format::Language::En.nature_name(&mon.nature),
                    if mon.displays_shiny() {
                        " · ✨ Shiny"
                    } else {
                        ""
                    }
                ))
            }))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if p.active.is_none() {
                        crate::pokemon::format::Language::En.egg_hint(&compact(target))
                    } else {
                        "Keep coding to evolve and graduate".into()
                    }),
            )
            .children(
                p.active
                    .as_ref()
                    .map(|mon| self.render_pokemon_evolution_line(mon, true, cx)),
            )
            .child(
                div()
                    .id("pokemon-progress")
                    .role(Role::ProgressIndicator)
                    .aria_label(format!(
                        "Growth: {:.0} percent, {used} of {target} tokens",
                        progress * 100.
                    ))
                    .h(px(6.))
                    .w_full()
                    .bg(cx.theme().muted)
                    .rounded_full()
                    .child(
                        div()
                            .h_full()
                            .w(relative(progress as f32))
                            .rounded_full()
                            .bg(cx.theme().primary),
                    )
                    .test_support(),
            )
            .child(
                div()
                    .id("pokemon-progress-detail")
                    .text_xs()
                    .child(crate::pokemon::format::progress_detail(used, target))
                    .test_support(),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{} collected · {} spendable tokens",
                        p.collection.len(),
                        compact(p.balance())
                    )),
            )
            .child(
                div()
                    .id("pokemon-status")
                    .text_xs()
                    .aria_label(self.pokemon_mood().label())
                    .child(if self.pokemon_ui.hatch_retry_delayed {
                        crate::pokemon::format::Language::En.egg_delayed()
                    } else {
                        self.pokemon_mood().label()
                    })
                    .test_support(),
            )
            .children(
                p.active
                    .as_ref()
                    .and_then(|mon| self.form_metadata_note(mon.species_id(), mon.form.as_ref()))
                    .map(|text| {
                        div()
                            .id("pokemon-home-form-metadata-note")
                            .text_xs()
                            .role(Role::Label)
                            .aria_label(text)
                            .child(text)
                            .test_support()
                    }),
            )
            .child(self.pokemon_difficulty(cx))
            .test_support()
            .into_any_element()
    }

    fn dex_query(&self, cx: &App) -> String {
        self.pokemon_ui
            .search
            .as_ref()
            .map(|s| s.read(cx).value().to_string())
            .unwrap_or_default()
    }

    fn dex_entries(&self, cx: &App) -> Vec<(u16, Mon)> {
        dex::species(
            &self.state.pokemon,
            &self.dex_query(cx),
            self.pokemon_ui.rarity,
            self.pokemon_ui.shiny_only,
            self.pokemon_ui.sort,
        )
    }

    fn load_dex_assets(&mut self, cx: &mut Context<Self>) {
        self.pokemon_ui.thumbnails.retain(|key, path| {
            crate::pokemon::sprites::is_exact_animated(path, key.0, key.1, key.2.as_ref())
        });
        self.pokemon_ui.thumbnail_failed.clear();
        self.pokemon_ui.name_failed.clear();
        self.request_dex_names(cx);
        self.fill_dex_assets(cx);
    }

    fn fill_dex_assets(&mut self, cx: &mut Context<Self>) {
        if self.state.pokemon_disabled {
            return;
        }
        if self.pokemon_ui.catch_log {
            for key in self
                .pokemon_ui
                .catch_visible
                .iter()
                .take(16)
                .cloned()
                .collect::<Vec<_>>()
            {
                self.load_pokemon_thumbnail(key, cx);
            }
            return;
        }
        let entries = self.dex_entries(cx);
        let page = self
            .pokemon_ui
            .dex_page
            .min(entries.len().saturating_sub(1) / 16);
        for (species, mon) in entries.into_iter().skip(page * 16).take(16) {
            self.load_pokemon_thumbnail(mon_sprite_identity(&mon, species), cx);
        }
    }

    fn load_pokemon_thumbnail(&mut self, key: SpriteIdentity, cx: &mut Context<Self>) {
        if self.state.pokemon_disabled
            || self.pokemon_ui.thumbnails.contains_key(&key)
            || self.pokemon_ui.thumbnail_failed.contains(&key)
            || self.pokemon_ui.thumbnail_pending.len() >= 16
            || !self.pokemon_ui.thumbnail_pending.insert(key.clone())
        {
            return;
        }
        #[cfg(test)]
        if !self.usage_live {
            return;
        }
        let generation = self.pokemon_ui.generation;
        let (species, shiny, form) = key.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { api().cached_sprite_form(species, shiny, form.as_ref()) })
                .await;
            this.update(cx, |s, cx| {
                s.complete_pokemon_thumbnail(key, generation, result, cx);
            })
            .ok();
        })
        .detach();
    }

    fn complete_pokemon_thumbnail(
        &mut self,
        key: SpriteIdentity,
        generation: u64,
        result: Result<PathBuf, String>,
        cx: &mut Context<Self>,
    ) {
        if self.pokemon_ui.generation != generation {
            return;
        }
        self.pokemon_ui.thumbnail_pending.remove(&key);
        match result {
            Ok(path) => {
                self.pokemon_ui.thumbnails.insert(key, path);
            }
            Err(_) => {
                self.pokemon_ui.thumbnail_failed.insert(key);
            }
        }
        // Refill only the currently visible page. Failed assets wait for an explicit
        // retry/navigation instead of creating a tight background retry loop.
        if self.pokemon_ui.tab == Page::Dex {
            self.fill_dex_assets(cx);
        }
        if self
            .usage_panel
            .as_ref()
            .is_some_and(|panel| panel.recap.is_some())
        {
            self.warm_usage_recap(cx);
        }
        cx.notify();
    }

    fn select_pokemon(&mut self, species: u16, cx: &mut Context<Self>) {
        if self.pokemon_ui.tab != Page::Dex {
            self.pokemon_ui.catch_log = false;
            self.pokemon_ui.dex_page = dex::containing(
                Some(species),
                self.dex_entries(cx).into_iter().map(|(id, _)| id),
            );
        }
        let from_home = self.pokemon_ui.tab == Page::Home;
        self.pokemon_ui.detail_form = if from_home {
            self.state
                .pokemon
                .active
                .as_ref()
                .filter(|m| m.species_id() == species)
                .and_then(|m| crate::pokemon::forms::resolved(species, m.form.as_ref()))
        } else if self.state.pokemon.representative == Some(species) {
            self.state.pokemon.representative_sprite_form()
        } else {
            None
        };
        self.pokemon_ui
            .evolution_detail_scroll
            .set_offset(point(px(0.), px(0.)));
        self.pokemon_ui.selected_species = Some(species);
        let preferred_shiny = if from_home {
            self.state
                .pokemon
                .active
                .as_ref()
                .is_some_and(|m| m.displays_shiny())
        } else {
            false
        };
        self.pokemon_ui.individual = self
            .detail_individuals(species)
            .iter()
            .position(|m| m.displays_shiny() == preferred_shiny)
            .unwrap_or(0);
        if let Some(mon) = self.selected_pokemon_individual(species) {
            self.load_pokemon_thumbnail(mon_sprite_identity(mon, species), cx);
        }
        self.pokemon_ui.tab = Page::Details;
        self.pokemon_ui.name_failed.clear();
        self.request_dex_names(cx);
        self.sync_profile_names(true, cx);
        self.request_pokemon_details(species, cx);
        cx.notify();
    }

    fn request_pokemon_details(&mut self, species: u16, cx: &mut Context<Self>) {
        if self.state.pokemon_disabled {
            return;
        }
        if let Some(details) = self.pokemon_ui.details.get(&species) {
            self.state.pokemon.enrich_profiles(details);
            self.save_pokemon();
            self.sync_profile_names(false, cx);
            return;
        }
        if self.pokemon_ui.detail_pending.contains(&species) {
            return;
        }
        if self.pokemon_ui.detail_pending.len() >= 4 {
            // Navigation keeps only the latest desired profile rather than queuing
            // every briefly selected species behind four background requests.
            self.pokemon_ui.detail_deferred = Some(species);
            return;
        }
        self.pokemon_ui.detail_pending.insert(species);
        #[cfg(test)]
        if !self.usage_live {
            return;
        }
        let generation = self.pokemon_ui.generation;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { api().details(species) })
                .await;
            this.update(cx, |s, cx| {
                s.complete_pokemon_details(species, generation, result, cx)
            })
            .ok();
        })
        .detach();
    }

    fn complete_pokemon_details(
        &mut self,
        species: u16,
        generation: u64,
        result: Result<profile::Details, String>,
        cx: &mut Context<Self>,
    ) {
        if self.pokemon_ui.generation != generation {
            return;
        }
        self.pokemon_ui.detail_pending.remove(&species);
        match result {
            Ok(details) => {
                self.state.pokemon.enrich_profiles(&details);
                self.pokemon_ui.details.insert(species, details);
                self.save_pokemon();
            }
            Err(error) => {
                self.pokemon_ui.notice = Some(format!("Could not load Pokémon details: {error}"))
            }
        }
        if let Some(species) = self.pokemon_ui.detail_deferred.take() {
            self.request_pokemon_details(species, cx);
        }
        self.sync_profile_names(false, cx);
        cx.notify();
    }

    fn pokemon_dex(&self, cx: &Context<Self>) -> AnyElement {
        let entries = self.dex_entries(cx);
        let pages = entries.len().max(1).div_ceil(16);
        let page = self.pokemon_ui.dex_page.min(pages - 1);
        v_flex()
            .id("pokemon-dex")
            .gap_2()
            .child(
                h_flex().gap_1().children(
                    [(false, "Pokédex"), (true, "Catch log")]
                        .into_iter()
                        .map(|(log, label)| {
                            Button::new(if log {
                                "pokemon-segment-log"
                            } else {
                                "pokemon-segment-dex"
                            })
                            .ghost()
                            .xsmall()
                            .label(label)
                            .selected(self.pokemon_ui.catch_log == log)
                            .on_click(cx.listener(
                                move |s, _, _, cx| {
                                    s.pokemon_ui.catch_log = log;
                                    s.pokemon_ui.dex_page = 0;
                                    s.pokemon_ui
                                        .catch_scroll
                                        .0
                                        .borrow()
                                        .base_handle
                                        .set_offset(point(px(0.), px(0.)));
                                    s.load_dex_assets(cx);
                                    cx.notify();
                                },
                            ))
                        }),
                ),
            )
            .children(self.pokemon_ui.search.as_ref().map(|state| {
                div()
                    .id("pokemon-dex-search")
                    .test_support()
                    .child(Input::new(state).small())
            }))
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("pokemon-filter-rarity")
                            .ghost()
                            .xsmall()
                            .label(self.pokemon_ui.rarity.map_or("All rarities", Rarity::label))
                            .on_click(cx.listener(|s, _, _, cx| {
                                s.pokemon_ui.rarity = match s.pokemon_ui.rarity {
                                    None => Some(Rarity::Common),
                                    Some(Rarity::Common) => Some(Rarity::Uncommon),
                                    Some(Rarity::Uncommon) => Some(Rarity::Rare),
                                    Some(Rarity::Rare) => Some(Rarity::Legendary),
                                    _ => None,
                                };
                                s.pokemon_ui.dex_page = 0;
                                s.load_dex_assets(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("pokemon-filter-shiny")
                            .ghost()
                            .xsmall()
                            .label("Shiny")
                            .selected(self.pokemon_ui.shiny_only)
                            .on_click(cx.listener(|s, _, _, cx| {
                                s.pokemon_ui.shiny_only = !s.pokemon_ui.shiny_only;
                                s.pokemon_ui.dex_page = 0;
                                s.load_dex_assets(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("pokemon-sort")
                            .ghost()
                            .xsmall()
                            .label(if self.pokemon_ui.catch_log {
                                self.pokemon_ui
                                    .catch_sort
                                    .unwrap_or(dex::Sort::RecentFirst)
                                    .label()
                            } else {
                                self.pokemon_ui.sort.label()
                            })
                            .on_click(cx.listener(|s, _, _, cx| {
                                if s.pokemon_ui.catch_log {
                                    s.pokemon_ui.catch_sort = Some(
                                        s.pokemon_ui
                                            .catch_sort
                                            .unwrap_or(dex::Sort::RecentFirst)
                                            .next(true),
                                    );
                                } else {
                                    s.pokemon_ui.sort = s.pokemon_ui.sort.next(false);
                                }
                                s.pokemon_ui.dex_page = 0;
                                s.load_dex_assets(cx);
                                cx.notify();
                            })),
                    ),
            )
            .child(
                Button::new("pokemon-clear-filters")
                    .ghost()
                    .xsmall()
                    .label("Clear filters")
                    .on_click(cx.listener(|s, _, window, cx| {
                        s.pokemon_ui.rarity = None;
                        s.pokemon_ui.shiny_only = false;
                        s.pokemon_ui.dex_page = 0;
                        if let Some(search) = s.pokemon_ui.search.clone() {
                            search.update(cx, |input, cx| input.set_value("", window, cx));
                        }
                        s.load_dex_assets(cx);
                        cx.notify();
                    })),
            )
            .when(entries.is_empty() && !self.pokemon_ui.catch_log, |d| {
                d.child("No Pokémon match. Hatch an egg to begin your collection.")
            })
            .when(!self.pokemon_ui.catch_log, |d| {
                d.child(
                    div()
                        .id("pokemon-dex-grid")
                        .flex()
                        .flex_wrap()
                        .gap_1()
                        .on_scroll_wheel(cx.listener(move |s, event: &ScrollWheelEvent, _, cx| {
                            let (dy, precise) = match event.delta {
                                ScrollDelta::Pixels(p) => (f32::from(p.y), true),
                                ScrollDelta::Lines(p) => (p.y, false),
                            };
                            let step = s.pokemon_ui.pager.step(dy, precise, event.touch_phase);
                            let next = dex::page_after(page, pages, step);
                            if next != page {
                                s.pokemon_ui.dex_page = next;
                                s.load_dex_assets(cx);
                                cx.notify();
                            }
                            cx.stop_propagation();
                        }))
                        .test_support()
                        .children(entries.into_iter().skip(page * 16).take(16).map(
                            |(species, mon)| {
                                let name = mon.line.name(species);
                                let art = match self
                                    .pokemon_ui
                                    .thumbnails
                                    .get(&mon_sprite_identity(&mon, species))
                                {
                                    Some(path) => div()
                                        .size(px(48.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            images::sized(path, 48.)
                                                .id(format!("pokemon-dex-sprite-{species}")),
                                        )
                                        .into_any_element(),
                                    None => div()
                                        .size(px(48.))
                                        .flex()
                                        .justify_center()
                                        .items_center()
                                        .child(format!("#{species:03}"))
                                        .into_any_element(),
                                };
                                Button::new(format!("pokemon-dex-{species}"))
                                    .ghost()
                                    .h_auto()
                                    .w(px(70.))
                                    .p_1()
                                    .accessibility_label(format!("Open {name} details"))
                                    .tooltip(name.clone())
                                    .on_click(
                                        cx.listener(move |s, _, _, cx| {
                                            s.select_pokemon(species, cx)
                                        }),
                                    )
                                    .child(
                                        v_flex()
                                            .w_full()
                                            .items_center()
                                            .child(art)
                                            .child(
                                                div()
                                                    .id(format!("pokemon-dex-name-{species}"))
                                                    .w_full()
                                                    .text_xs()
                                                    .text_center()
                                                    .text_ellipsis()
                                                    .overflow_hidden()
                                                    .role(Role::Label)
                                                    .aria_label(name.clone())
                                                    .child(name)
                                                    .test_support(),
                                            )
                                            .when(
                                                self.state.pokemon.active.as_ref().is_some_and(
                                                    |active| active.species_id() == species,
                                                ),
                                                |d| {
                                                    d.child(
                                                        div().text_xs().child(
                                                            crate::pokemon::format::Language::En
                                                                .dex_raising(),
                                                        ),
                                                    )
                                                },
                                            )
                                            .when(mon.displays_shiny(), |d| {
                                                d.child(div().text_xs().child("✨"))
                                            }),
                                    )
                            },
                        )),
                )
            })
            .when(!self.pokemon_ui.catch_log, |d| {
                d.child(
                    h_flex()
                        .justify_between()
                        .child(
                            Button::new("pokemon-page-previous")
                                .h(px(28.))
                                .ghost()
                                .small()
                                .label("Previous")
                                .disabled(page == 0)
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    s.pokemon_ui.dex_page = page.saturating_sub(1);
                                    s.load_dex_assets(cx);
                                    cx.notify();
                                })),
                        )
                        .child(format!("{} / {pages}", page + 1))
                        .child(
                            Button::new("pokemon-page-next")
                                .h(px(28.))
                                .ghost()
                                .small()
                                .label("Next")
                                .disabled(page + 1 >= pages)
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    s.pokemon_ui.dex_page = (page + 1).min(pages - 1);
                                    s.load_dex_assets(cx);
                                    cx.notify();
                                })),
                        ),
                )
            })
            .when(self.pokemon_ui.catch_log, |d| {
                d.child(self.pokemon_catch_log(cx))
            })
            .child(
                Button::new("pokemon-follow")
                    .ghost()
                    .small()
                    .label(crate::pokemon::format::Language::En.interface_text(
                        crate::pokemon::interface_labels::Label::RepresentativeFollowCurrent,
                    ))
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.state.pokemon.pin(None);
                        s.save_pokemon();
                        s.load_pokemon(cx);
                        cx.notify();
                    })),
            )
            .test_support()
            .into_any_element()
    }

    fn pokemon_details(&self, cx: &Context<Self>) -> AnyElement {
        let Some(species) = self.pokemon_ui.selected_species else {
            return div().into_any_element();
        };
        let Some(mon) = self.selected_pokemon_individual(species) else {
            return div()
                .child("Pokémon no longer in collection")
                .into_any_element();
        };
        // Prior evolution artwork is owned, but its final-form profile is not a prior-form individual.
        let mut profile = (mon.species_id() == species)
            .then(|| mon.profile.clone())
            .flatten();
        if let (Some(profile), Some(details)) =
            (&mut profile, self.pokemon_ui.details.get(&species))
        {
            profile.enrich(details);
        }
        let selected_form = mon.form.clone();
        let selected_shiny = mon.displays_shiny();
        let stats = profile
            .as_ref()
            .zip(self.pokemon_ui.details.get(&species))
            .map(|(p, d)| profile::computed_stats(d, p, Some(&mon.nature.to_lowercase())));
        let ability_label = format!(
            "Ability: {}",
            profile
                .as_ref()
                .and_then(|p| p.ability_name.as_ref().map(|name| {
                    self.resource_text(&[NameItem {
                        resource: NameResource::new(NameKind::Ability, name),
                        suffix: if p.ability_is_hidden {
                            " (Hidden)".into()
                        } else {
                            String::new()
                        },
                    }])
                }))
                .unwrap_or_else(|| "Loading…".into())
        );
        let moves_label = format!(
            "Moves: {}",
            profile
                .as_ref()
                .filter(|p| !p.moves.is_empty())
                .map(|p| {
                    let items = p
                        .moves
                        .iter()
                        .map(|m| NameItem {
                            resource: NameResource::new(NameKind::Move, &m.name),
                            suffix: String::new(),
                        })
                        .collect::<Vec<_>>();
                    self.resource_text(&items)
                })
                .unwrap_or_else(|| "None yet".into())
        );
        v_flex()
            .id("pokemon-details")
            .gap_2()
            .child(
                Button::new("pokemon-dex-back")
                    .ghost()
                    .small()
                    .label("Back to Pokédex")
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.pokemon_ui.tab = Page::Dex;
                        s.pokemon_ui.selected_species = None;
                        s.sync_profile_names(false, cx);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!(
                        "#{species:03} {}{}",
                        mon.line.name(species),
                        if mon.displays_shiny() { " ✨" } else { "" }
                    )),
            )
            .child(format!(
                "{} · {} nature",
                mon.line.rarity.label(),
                crate::pokemon::format::Language::En.nature_name(&mon.nature)
            ))
            .children(
                self.selected_form_label(species, mon.form.as_ref())
                    .map(|label| {
                        div()
                            .id("pokemon-form-name")
                            .text_xs()
                            .role(Role::Label)
                            .aria_label(format!("Form: {label}"))
                            .child(format!("Form: {label}"))
                            .test_support()
                    }),
            )
            .child(self.render_pokemon_appearance_picker(species, mon, cx))
            .children(
                self.form_metadata_note(species, mon.form.as_ref())
                    .map(|text| {
                        div()
                            .id("pokemon-form-metadata-note")
                            .text_xs()
                            .role(Role::Label)
                            .aria_label(text)
                            .child(text)
                            .test_support()
                    }),
            )
            .children(profile.as_ref().map(|p| {
                v_flex()
                    .gap_1()
                    .child(format!(
                        "Level {} · {}",
                        p.level,
                        match p.gender {
                            Some(profile::Gender::Male) => "Male",
                            Some(profile::Gender::Female) => "Female",
                            Some(profile::Gender::Genderless) => "Genderless",
                            None => "Gender pending",
                        }
                    ))
                    .child(
                        div()
                            .id("pokemon-ability-name")
                            .role(Role::Label)
                            .aria_label(ability_label.clone())
                            .child(ability_label.clone())
                            .test_support(),
                    )
                    .child(div().text_xs().child(format!(
                        "IVs: HP {} · ATK {} · DEF {} · SpA {} · SpD {} · SPD {}",
                        p.ivs[0], p.ivs[1], p.ivs[2], p.ivs[3], p.ivs[4], p.ivs[5]
                    )))
                    .children(stats.as_ref().map(|values| {
                        div().text_xs().child(format!(
                            "Stats: {}",
                            profile::STAT_ORDER
                                .iter()
                                .filter_map(|key| values.get(*key).map(|v| format!("{key} {v}")))
                                .collect::<Vec<_>>()
                                .join(" · ")
                        ))
                    }))
                    .child(
                        div()
                            .id("pokemon-move-names")
                            .text_xs()
                            .role(Role::Label)
                            .aria_label(moves_label.clone())
                            .child(moves_label.clone())
                            .test_support(),
                    )
            }))
            .children(self.pokemon_ui.details.get(&species).map(|details| {
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .children(details.types.iter().map(|name| {
                        let tint: Hsla = rgb(crate::pokemon::types::color(name)).into();
                        div()
                            .id(format!("pokemon-type-{name}"))
                            .px_2()
                            .py_0p5()
                            .rounded_full()
                            .text_xs()
                            .bg(tint.opacity(0.18))
                            .border_1()
                            .border_color(tint)
                            .role(Role::Label)
                            .aria_label(self.resource_label(NameKind::Type, name))
                            .child(self.resource_label(NameKind::Type, name))
                            .test_support()
                    }))
            }))
            .child(self.render_pokemon_evolution_line(mon, false, cx))
            .child(
                Button::new("pokemon-pin-selected")
                    .small()
                    .w(px(28.))
                    .h(px(28.))
                    .icon(IconName::Check)
                    .accessibility_label(
                        crate::pokemon::format::Language::En.interface_text(
                            crate::pokemon::interface_labels::Label::RepresentativeSet,
                        ),
                    )
                    .tooltip(
                        crate::pokemon::format::Language::En.interface_text(
                            crate::pokemon::interface_labels::Label::RepresentativeSet,
                        ),
                    )
                    .on_click(cx.listener(move |s, _, _, cx| {
                        s.state.pokemon.pin_appearance(
                            species,
                            selected_shiny,
                            selected_form.clone(),
                        );
                        s.save_pokemon();
                        s.load_pokemon(cx);
                        cx.notify();
                    })),
            )
            .test_support()
            .into_any_element()
    }

    fn pokemon_difficulty(&self, cx: &Context<Self>) -> AnyElement {
        let growth = self
            .pokemon_ui
            .growth_draft
            .unwrap_or(self.state.pokemon.growth_difficulty);
        let shop = self
            .pokemon_ui
            .shop_draft
            .unwrap_or(self.state.pokemon.shop_difficulty);
        v_flex()
            .w_full()
            .gap_1()
            .mt_2()
            .child(div().text_xs().child("Difficulty"))
            .children(
                [(true, "Growth", growth), (false, "Shop prices", shop)]
                    .into_iter()
                    .map(|(is_growth, label, value)| {
                        h_flex()
                            .gap_1()
                            .justify_between()
                            .child(
                                Button::new(format!("pokemon-{label}-less"))
                                    .ghost()
                                    .xsmall()
                                    .label("−")
                                    .disabled(value <= 0.1)
                                    .on_click(cx.listener(move |s, _, _, cx| {
                                        let draft = Balance::clamp_difficulty(
                                            (value * 10. - 1.).round() / 10.,
                                        );
                                        if is_growth {
                                            s.pokemon_ui.growth_draft = Some(draft);
                                        } else {
                                            s.pokemon_ui.shop_draft = Some(draft);
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .child(format!("{label}: {:.0}%", value * 100.)),
                            )
                            .child(
                                Button::new(format!("pokemon-{label}-more"))
                                    .ghost()
                                    .xsmall()
                                    .label("+")
                                    .disabled(value >= 2.)
                                    .on_click(cx.listener(move |s, _, _, cx| {
                                        let draft = Balance::clamp_difficulty(
                                            (value * 10. + 1.).round() / 10.,
                                        );
                                        if is_growth {
                                            s.pokemon_ui.growth_draft = Some(draft);
                                        } else {
                                            s.pokemon_ui.shop_draft = Some(draft);
                                        }
                                        cx.notify();
                                    })),
                            )
                    }),
            )
            .when(
                growth != self.state.pokemon.growth_difficulty
                    || shop != self.state.pokemon.shop_difficulty,
                |d| {
                    d.child(
                        Button::new("pokemon-save-difficulty")
                            .ghost()
                            .small()
                            .label("Save difficulty")
                            .disabled(
                                growth == self.state.pokemon.growth_difficulty
                                    && shop == self.state.pokemon.shop_difficulty,
                            )
                            .on_click(cx.listener(move |s, _, _, cx| {
                                s.state.pokemon.set_growth_difficulty(growth);
                                s.state.pokemon.set_shop_difficulty(shop);
                                s.pokemon_ui.growth_draft = None;
                                s.pokemon_ui.shop_draft = None;
                                s.save_pokemon();
                                cx.notify();
                            })),
                    )
                },
            )
            .into_any_element()
    }

    fn pokemon_quantity(&self, cx: &Context<Self>) -> AnyElement {
        let quantity = self.pokemon_ui.quantity.max(1);
        h_flex()
            .gap_2()
            .justify_between()
            .child(
                Button::new("pokemon-quantity-less")
                    .ghost()
                    .xsmall()
                    .label("−")
                    .disabled(quantity <= 1)
                    .on_click(cx.listener(move |s, _, _, cx| {
                        s.pokemon_ui.quantity = quantity.saturating_sub(1).max(1);
                        cx.notify();
                    })),
            )
            .child(div().text_xs().child(format!("Quantity: {quantity}")))
            .child(
                Button::new("pokemon-quantity-more")
                    .ghost()
                    .xsmall()
                    .label("+")
                    .disabled(quantity >= 99)
                    .on_click(cx.listener(move |s, _, _, cx| {
                        s.pokemon_ui.quantity = (quantity + 1).min(99);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn pokemon_bag(&self, cx: &Context<Self>) -> AnyElement {
        let bag = &self.state.pokemon.inventory;
        v_flex()
            .id("pokemon-bag")
            .gap_2()
            .child(self.pokemon_quantity(cx))
            .child(format!("Rare Candy × {}", bag.candies))
            .child(
                Button::new("pokemon-use-candy")
                    .small()
                    .label("Use Rare Candy")
                    .disabled(bag.candies == 0)
                    .on_click(cx.listener(|s, _, _, cx| {
                        match s.state.pokemon.use_candy(
                            s.pokemon_ui
                                .quantity
                                .max(1)
                                .min(s.state.pokemon.inventory.candies),
                        ) {
                            Ok(events) => s.pokemon_events(events),
                            Err(e) => s.pokemon_ui.notice = Some(e),
                        }
                        s.save_pokemon();
                        s.load_pokemon(cx);
                        cx.notify();
                    })),
            )
            .child(format!("Mint × {}", bag.mints))
            .child(
                Button::new("pokemon-use-mint")
                    .small()
                    .label("Use Mint")
                    .disabled(bag.mints == 0 || self.state.pokemon.active.is_none())
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.pokemon_ui.notice = Some(match s.state.pokemon.use_mint() {
                            Ok(()) => "Your Pokémon has a new nature.".into(),
                            Err(e) => e,
                        });
                        s.save_pokemon();
                        cx.notify();
                    })),
            )
            .child(if bag.shiny_charm {
                "Shiny Charm active"
            } else {
                "No Shiny Charm yet"
            })
            .test_support()
            .into_any_element()
    }

    fn pokemon_shop(&self, cx: &Context<Self>) -> AnyElement {
        let p = &self.state.pokemon;
        v_flex()
            .id("pokemon-shop")
            .gap_2()
            .child(format!("Balance: {} tokens", compact(p.balance())))
            .child(self.pokemon_quantity(cx))
            .children(
                [Item::RareCandy, Item::Mint, Item::ShinyCharm]
                    .into_iter()
                    .map(|item| {
                        let label = crate::pokemon::format::Language::En.item_name(item);
                        let quantity = if item == Item::ShinyCharm {
                            1
                        } else {
                            self.pokemon_ui.quantity.max(1)
                        };
                        let price = p.price(item).saturating_mul(quantity);
                        Button::new(format!("pokemon-buy-{label}"))
                            .small()
                            .label(format!("{label} · {}", compact(price)))
                            .disabled(
                                p.balance() < price
                                    || (item == Item::ShinyCharm && p.inventory.shiny_charm),
                            )
                            .on_click(cx.listener(move |s, _, _, cx| {
                                s.pokemon_ui.notice =
                                    Some(match s.state.pokemon.buy(item, quantity) {
                                        Ok(()) => format!("Bought {label}. Find it in your Bag."),
                                        Err(e) => e,
                                    });
                                s.save_pokemon();
                                cx.notify();
                            }))
                    }),
            )
            .child(
                div()
                    .mt_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Buying an egg archives your current companion and starts a new egg."),
            )
            .children(
                [
                    (None, "Pokémon Egg"),
                    (Some(Rarity::Uncommon), "Uncommon Egg"),
                    (Some(Rarity::Rare), "Rare Egg"),
                ]
                .into_iter()
                .map(|(tier, label)| {
                    let price = p.egg_price(tier);
                    Button::new(format!("pokemon-buy-{label}"))
                        .small()
                        .label(format!("{label} · {}", compact(price)))
                        .disabled(p.balance() < price)
                        .on_click(cx.listener(move |s, _, window, cx| {
                            s.review_pokemon_egg(tier, window, cx);
                        }))
                }),
            )
            .test_support()
            .into_any_element()
    }
}

#[cfg(test)]
mod sprite_tests {
    use super::*;
    use core::prelude::v1::test;

    // upstream: ShinyCharmTests.swift::testShinyFlipReloadsSpriteForSameSpecies
    #[test]
    fn same_species_shiny_flip_reloads_sprite() {
        assert!(sprite_needs_reload(
            &Some((25, true, None)),
            true,
            &Some((25, false, None))
        ));
        assert!(sprite_needs_reload(
            &Some((25, false, None)),
            true,
            &Some((25, true, None))
        ));
    }
    // upstream: ShinyCharmTests.swift::testUnchangedSpriteSkipsReload
    #[test]
    fn unchanged_species_and_shiny_does_not_reload_sprite() {
        assert!(!sprite_needs_reload(
            &Some((25, false, None)),
            true,
            &Some((25, false, None))
        ));
        assert!(!sprite_needs_reload(
            &Some((25, true, None)),
            true,
            &Some((25, true, None))
        ));
    }
    // upstream: ShinyCharmTests.swift::testSpeciesChangeReloadsRegardlessOfShiny
    #[test]
    fn changed_species_reloads_sprite() {
        assert!(sprite_needs_reload(&None, false, &Some((25, false, None))));
        assert!(sprite_needs_reload(
            &Some((25, false, None)),
            true,
            &Some((26, true, None))
        ));
        assert!(sprite_needs_reload(
            &Some((25, false, None)),
            true,
            &Some((26, false, None))
        ));
    }
    #[test]
    fn changed_form_reloads_sprite() {
        assert!(sprite_needs_reload(
            &Some((422, false, Some(crate::pokemon::forms::Form("west".into())))),
            true,
            &Some((422, false, Some(crate::pokemon::forms::Form("east".into()))))
        ));
    }
    #[test]
    fn failed_image_retries_and_new_egg_clears_old_sprite() {
        assert!(sprite_needs_reload(
            &Some((25, false, None)),
            false,
            &Some((25, false, None))
        ));
        assert!(sprite_needs_reload(&Some((25, false, None)), true, &None));
        assert!(!sprite_needs_reload(&None, false, &None));
    }
}

#[cfg(all(test, feature = "test-support"))]
mod async_tests {
    use super::*;
    use crate::app::ui_tests::{UI_LOCK, boot, test_home};
    use core::prelude::v1::test;

    fn line(id: u16) -> crate::pokemon::EvoLine {
        crate::pokemon::EvoLine {
            base_id: id,
            tree: crate::pokemon::EvoNode {
                species_id: id,
                children: vec![],
            },
            rarity: Rarity::Common,
            names: BTreeMap::from([(id, BTreeMap::from([("en".into(), format!("Species {id}"))]))]),
        }
    }
    fn hatched(id: u16) -> Companion {
        let mut p = Companion::default();
        p.egg_tokens = Balance::EGG_HATCH_THRESHOLD;
        p.hatch(line(id));
        p
    }

    // upstream: PokemonProfileTests.swift::testDetailFetchFailureSetsRetryStateAndClearsLoadingState
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn detail_failure_clears_loading_and_can_retry(cx: &mut TestAppContext) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        test_home();
        let mut state = State::default();
        state.pokemon = hatched(25);
        let (window, shark) = boot(cx, state, WindowOptions::default());
        cx.update_window(window, |_, _, cx| {
            shark.update(cx, |s, cx| {
                s.request_pokemon_details(25, cx);
                assert!(s.pokemon_ui.detail_pending.contains(&25));
                s.complete_pokemon_details(25, s.pokemon_ui.generation, Err("offline".into()), cx);
                assert!(!s.pokemon_ui.detail_pending.contains(&25));
                assert!(s.pokemon_ui.notice.as_ref().unwrap().contains("offline"));
                s.request_pokemon_details(25, cx);
                assert!(s.pokemon_ui.detail_pending.contains(&25));
            })
        })
        .unwrap();
    }

    // upstream: PokemonProfileTests.swift::testStartupWarmupLoadsOnlyTheActivePokemon
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn startup_warms_only_active_profile(cx: &mut TestAppContext) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        test_home();
        let mut state = State::default();
        state.pokemon = hatched(25);
        state
            .pokemon
            .collection
            .push(hatched(1).active.take().unwrap());
        let (window, shark) = boot(cx, state, WindowOptions::default());
        cx.update_window(window, |_, _, cx| {
            shark.update(cx, |s, cx| {
                s.load_pokemon(cx);
                assert_eq!(s.pokemon_ui.detail_pending, HashSet::from([25]));
                assert!(s.pokemon_ui.details.is_empty());
            })
        })
        .unwrap();
    }

    // upstream: PokemonProfileTests.swift::testLineLoadReleasesHatchingLockBeforeDetailRequestCompletes
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn hatch_releases_line_lock_before_profile_request_finishes(cx: &mut TestAppContext) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        test_home();
        let (window, shark) = boot(cx, State::default(), WindowOptions::default());
        cx.update_window(window, |_, _, cx| {
            shark.update(cx, |s, cx| {
                s.state.pokemon.egg_tokens = Balance::EGG_HATCH_THRESHOLD;
                s.pokemon_ui.hatch_pending = true;
                s.complete_pokemon_hatch(s.pokemon_ui.generation, None, false, Ok(line(25)), cx);
                assert!(!s.pokemon_ui.hatch_pending);
                assert_eq!(s.state.pokemon.active.as_ref().unwrap().species_id(), 25);
                assert!(s.pokemon_ui.detail_pending.contains(&25));
                assert!(s.pokemon_ui.details.is_empty());
            })
        })
        .unwrap();
    }

    // upstream: DittoTests.swift::testDelayedRevealDoesNotConvertSameBaseReplacementDisguise
    // upstream: SaveTransferTests.swift::testImportDuringHatchDiscardsTheHatch
    // upstream: SaveTransferTests.swift::testImportDuringSpeciesRollDiscardsTheHatch
    // upstream: SaveTransferTests.swift::testImportDuringHatchStillLoadsTheImportedLine
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn stale_hatch_and_detail_completions_cannot_change_imported_save(
        cx: &mut TestAppContext,
    ) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        test_home();
        let (window, shark) = boot(cx, State::default(), WindowOptions::default());
        cx.update_window(window, |_, _, cx| {
            shark.update(cx, |s, cx| {
                let old = s.pokemon_ui.generation;
                s.pokemon_ui.hatch_pending = true;
                s.apply_pokemon_save(hatched(7), cx);
                assert!(!s.pokemon_ui.hatch_pending);
                assert_eq!(s.state.pokemon.active.as_ref().unwrap().line.base_id, 7);
                assert!(s.pokemon_ui.detail_pending.contains(&7));
                let before = serde_json::to_value(&s.state.pokemon).unwrap();
                s.complete_pokemon_hatch(old, None, false, Ok(line(25)), cx);
                s.complete_pokemon_hatch(old, None, false, Err("stale failure".into()), cx);
                s.complete_pokemon_details(7, old, Err("stale failure".into()), cx);
                assert_eq!(before, serde_json::to_value(&s.state.pokemon).unwrap());
                assert!(s.pokemon_ui.notice.is_none());
                let old_individual = hatched(7).active.unwrap().profile.unwrap().instance_id;
                let mon = s.state.pokemon.active.as_mut().unwrap();
                mon.ditto_disguise = Some(7);
                mon.tokens = mon.threshold(s.state.pokemon.growth_difficulty);
                assert!(s.state.pokemon.ready_to_reveal());
                let id = s
                    .state
                    .pokemon
                    .active
                    .as_ref()
                    .unwrap()
                    .profile
                    .as_ref()
                    .unwrap()
                    .instance_id
                    .clone();
                s.complete_pokemon_hatch(
                    s.pokemon_ui.generation,
                    Some(&old_individual),
                    true,
                    Ok(line(132)),
                    cx,
                );
                assert_eq!(
                    s.state
                        .pokemon
                        .active
                        .as_ref()
                        .unwrap()
                        .profile
                        .as_ref()
                        .unwrap()
                        .instance_id,
                    id
                );
                assert_eq!(s.state.pokemon.active.as_ref().unwrap().species_id(), 7);
            })
        })
        .unwrap();
    }
    // upstream: PokemonProfileTests.swift::testPathNormalizationLoadsDetailsForTheRecoveredCurrentSpecies
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn normalized_path_requests_recovered_species_and_persists_details(
        cx: &mut TestAppContext,
    ) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        test_home();
        let (window, shark) = boot(cx, State::default(), WindowOptions::default());
        cx.update_window(window, |_, _, cx| {
            shark.update(cx, |s, cx| {
                let mut imported = hatched(79);
                let mon = imported.active.as_mut().unwrap();
                mon.path = vec![79, 999];
                mon.stage = 1;
                s.apply_pokemon_save(imported, cx);
                assert_eq!(s.state.pokemon.active.as_ref().unwrap().species_id(), 79);
                assert_eq!(s.pokemon_ui.detail_pending, HashSet::from([79]));
                let identity = s
                    .state
                    .pokemon
                    .active
                    .as_ref()
                    .unwrap()
                    .profile
                    .as_ref()
                    .unwrap()
                    .instance_id
                    .clone();
                s.complete_pokemon_details(
                    79,
                    s.pokemon_ui.generation,
                    Ok(profile::Details {
                        species_id: 79,
                        types: vec![],
                        gender_rate: -1,
                        base_stats: profile::STAT_ORDER
                            .into_iter()
                            .map(|key| (key.into(), 50))
                            .collect(),
                        abilities: vec![],
                        moves: vec![],
                    }),
                    cx,
                );
                let saved = State::load();
                let profile = saved
                    .pokemon
                    .active
                    .as_ref()
                    .unwrap()
                    .profile
                    .as_ref()
                    .unwrap();
                assert_eq!(profile.gender, Some(profile::Gender::Genderless));
                assert_eq!(profile.instance_id, identity);
                assert!(!s.pokemon_ui.detail_pending.contains(&79));
            })
        })
        .unwrap();
    }

    // upstream: PokemonProfileTests.swift::testPokemonIndividualsIncludesSyntheticActiveAndMatchingStoredIndividuals
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn individual_picker_uses_ui_identity_without_changing_profile_identity(
        cx: &mut TestAppContext,
    ) {
        use gpui_kit::test::TestWindowExt;
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        test_home();
        let mut state = State::default();
        state.pokemon = hatched(79);
        let active_id = state
            .pokemon
            .active
            .as_ref()
            .unwrap()
            .profile
            .as_ref()
            .unwrap()
            .instance_id
            .clone();
        for at in [10, 20] {
            let mut mon = hatched(79).active.take().unwrap();
            mon.caught_at = at;
            mon.profile.as_mut().unwrap().instance_id = format!("stored-{at}");
            state.pokemon.collection.push(mon);
        }
        let (window, shark) = boot(cx, state, WindowOptions::default());
        cx.update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| {
                s.show_pokemon(w, cx);
                s.select_pokemon(79, cx);
            });
            w.render_frame(cx);
            assert!(w.find("pokemon-individual-0").visible());
            assert!(w.find("pokemon-individual-2").visible());
            w.click("pokemon-individual-1", cx);
            assert_eq!(shark.read(cx).pokemon_ui.individual, 1);
            let individuals = shark.read(cx).state.pokemon.individuals(79);
            assert_eq!(
                individuals[0].profile.as_ref().unwrap().instance_id,
                active_id
            );
            assert_eq!(
                individuals[1].profile.as_ref().unwrap().instance_id,
                "stored-20"
            );
            assert_eq!(
                individuals[2].profile.as_ref().unwrap().instance_id,
                "stored-10"
            );
        })
        .unwrap();
    }
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn navigation_bounds_background_asset_work_and_keeps_latest_details(
        cx: &mut TestAppContext,
    ) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        test_home();
        let (window, shark) = boot(cx, State::default(), WindowOptions::default());
        cx.update_window(window, |_, _, cx| {
            shark.update(cx, |s, cx| {
                for id in 1..=40 {
                    s.load_pokemon_thumbnail((id, false, None), cx);
                    s.request_pokemon_details(id, cx);
                }
                assert_eq!(s.pokemon_ui.thumbnail_pending.len(), 16);
                assert_eq!(s.pokemon_ui.detail_pending.len(), 4);
                assert_eq!(s.pokemon_ui.detail_deferred, Some(40));
                s.complete_pokemon_details(1, s.pokemon_ui.generation, Err("offline".into()), cx);
                assert_eq!(s.pokemon_ui.detail_pending.len(), 4);
                assert!(s.pokemon_ui.detail_pending.contains(&40));
                assert_eq!(s.pokemon_ui.detail_deferred, None);
                s.complete_pokemon_details(40, s.pokemon_ui.generation, Err("offline".into()), cx);
                assert_eq!(
                    s.pokemon_ui.detail_pending.len(),
                    3,
                    "failed selected profile cannot immediately retry itself"
                );
                s.set_pokemon_enabled(false, cx);
                s.load_pokemon_thumbnail((42, false, None), cx);
                s.request_pokemon_details(42, cx);
                s.load_pokemon_sprite(cx);
                assert!(s.pokemon_ui.thumbnail_pending.is_empty());
                assert!(s.pokemon_ui.detail_pending.is_empty());
                assert!(s.pokemon_ui.detail_deferred.is_none());
                assert!(!s.pokemon_ui.sprite_pending);
            });
        })
        .unwrap();
    }

    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn thumbnail_completion_refills_current_page_without_retrying_failures(
        cx: &mut TestAppContext,
    ) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        test_home();
        let mut state = State::default();
        state.pokemon.collection = (1..=40)
            .map(|id| hatched(id).active.take().unwrap())
            .collect();
        let (window, shark) = boot(cx, state, WindowOptions::default());
        cx.update_window(window, |_, _, cx| {
            shark.update(cx, |s, cx| {
                s.pokemon_ui.tab = Page::Dex;
                s.load_dex_assets(cx);
                assert_eq!(s.pokemon_ui.thumbnail_pending.len(), 16);
                s.pokemon_ui.dex_page = 1;
                s.load_dex_assets(cx);
                assert_eq!(s.pokemon_ui.thumbnail_pending.len(), 16);
                s.complete_pokemon_thumbnail(
                    (1, false, None),
                    s.pokemon_ui.generation,
                    Err("offline".into()),
                    cx,
                );
                assert_eq!(s.pokemon_ui.thumbnail_pending.len(), 16);
                assert!(s.pokemon_ui.thumbnail_pending.contains(&(17, false, None)));
                s.pokemon_ui.dex_page = 0;
                s.complete_pokemon_thumbnail(
                    (2, false, None),
                    s.pokemon_ui.generation,
                    Err("offline".into()),
                    cx,
                );
                assert!(!s.pokemon_ui.thumbnail_pending.contains(&(1, false, None)));
                assert!(!s.pokemon_ui.thumbnail_pending.contains(&(2, false, None)));
                let old = s.pokemon_ui.generation;
                s.set_pokemon_enabled(false, cx);
                s.complete_pokemon_thumbnail((17, false, None), old, Ok("stale.gif".into()), cx);
                assert!(s.pokemon_ui.thumbnail_pending.is_empty());
                assert!(!s.pokemon_ui.thumbnails.contains_key(&(17, false, None)));
            });
        })
        .unwrap();
    }
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn fresh_egg_releases_all_invalidated_asset_request_slots(cx: &mut TestAppContext) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        test_home();
        let (window, shark) = boot(cx, State::default(), WindowOptions::default());
        cx.update_window(window, |_, _, cx| {
            shark.update(cx, |s, cx| {
                s.state.pokemon = hatched(25);
                for id in 1..=40 {
                    s.load_pokemon_thumbnail((id, false, None), cx);
                    s.request_pokemon_details(id, cx);
                }
                let old = s.pokemon_ui.generation;
                s.state.pokemon.lifetime_tokens = s.state.pokemon.egg_price(None);
                s.start_pokemon_egg(None, cx);
                assert_ne!(s.pokemon_ui.generation, old);
                assert!(s.pokemon_ui.thumbnail_pending.is_empty());
                assert!(s.pokemon_ui.detail_pending.is_empty());
                assert!(s.pokemon_ui.detail_deferred.is_none());
                s.load_pokemon_thumbnail((1, false, None), cx);
                s.request_pokemon_details(1, cx);
                s.complete_pokemon_thumbnail((1, false, None), old, Ok("stale.gif".into()), cx);
                s.complete_pokemon_details(1, old, Err("stale failure".into()), cx);
                assert!(s.pokemon_ui.thumbnail_pending.contains(&(1, false, None)));
                assert!(s.pokemon_ui.detail_pending.contains(&1));
                assert!(!s.pokemon_ui.thumbnails.contains_key(&(1, false, None)));
                assert!(
                    !s.pokemon_ui
                        .notice
                        .as_ref()
                        .unwrap()
                        .contains("stale failure")
                );
            });
        })
        .unwrap();
    }
}

#[cfg(all(test, feature = "test-support"))]
#[path = "pokemon/async_parity_tests.rs"]
mod async_parity_tests;

#[cfg(all(test, feature = "test-support"))]
#[path = "pokemon/name_parity_tests.rs"]
mod name_parity_tests;

#[cfg(all(test, feature = "test-support"))]
#[path = "pokemon/resource_name_tests.rs"]
mod resource_name_tests;

#[cfg(test)]
mod reward_notice_tests;

#[cfg(all(test, feature = "test-support"))]
#[path = "pokemon/appearance_tests.rs"]
pub(super) mod appearance_tests;

#[cfg(all(test, feature = "test-support"))]
#[path = "pokemon/appearance_pixels.rs"]
pub(super) mod appearance_pixels;
