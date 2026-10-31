//! Settings dialog (cmd-,): general (stay awake, Vim mode), appearance (mode, light and dark theme, fonts)
//! and the git button's agent prompts. Preferences apply immediately; custom token-source folders
//! are saved explicitly so partially typed paths never start scans.
use crate::SwitchField;
use crate::app::Shark;
use crate::prompts::{self, PROMPTS};
use crate::state::{Appearance, State};
use crate::theme::{THEMES, theme_configs};
use crate::ui::{caption, card, dialog, faint, mode_pills, row, section};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::link::Link;
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme, Disableable as _, IndexPath, Sizable as _, Theme, ThemeConfig, WindowExt, h_flex,
    try_parse_color, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

pub fn open(state: &State, window: &mut Window, cx: &mut Context<Shark>) {
    let shark = cx.entity().downgrade();
    let view = cx.new(|cx| Settings::new(shark, state, window, cx));
    let focus = view.read(cx).focus.clone();
    window.open_dialog(cx, move |d, _, _| {
        // No dialog title: the page list and the page title say where you are.
        dialog(d).w(px(800.)).child(view.clone())
    });
    focus.focus(window, cx);
}

/// About dialog (Shark > About Shark): version, author, license and repo link.
pub fn about(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |d, _, cx| {
        dialog(d).title("Shark").w(px(640.)).child(
            v_flex()
                .gap_1()
                .child(format!("Version {}", env!("CARGO_PKG_VERSION")))
                .child("Shark by Kasper Munch Jensen")
                .child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child("GPL-3.0"),
                )
                .child(
                    Link::new("about-repo")
                        .href("https://github.com/KaffeDiem/shark")
                        .child("github.com/KaffeDiem/shark"),
                ),
        )
    });
}

type Choice = Entity<SelectState<SearchableVec<SharedString>>>;

/// A theme's colors for its preview card.
struct Swatch {
    bg: Hsla,
    sidebar: Hsla,
    border: Hsla,
    fg: Hsla,
    muted: Hsla,
    primary: Hsla,
}

impl Swatch {
    fn new(t: &ThemeConfig) -> Self {
        let c = &t.colors;
        let parse = |v: &Option<SharedString>| v.as_deref().and_then(|v| try_parse_color(v).ok());
        let bg = parse(&c.background).unwrap_or(if t.mode.is_dark() { black() } else { white() });
        let or_bg = |v| parse(v).unwrap_or(bg);
        Swatch {
            bg,
            sidebar: or_bg(&c.sidebar),
            border: or_bg(&c.border),
            fg: or_bg(&c.foreground),
            muted: or_bg(&c.muted_foreground),
            primary: or_bg(&c.primary),
        }
    }
}

pub struct Settings {
    shark: WeakEntity<Shark>,
    tab: usize,
    focus: FocusHandle,
    /// Per `THEMES` family, in order.
    light: Vec<Swatch>,
    dark: Vec<Swatch>,
    ui_font: Choice,
    mono_font: Choice,
    prompts: Vec<Entity<TextareaState>>,
    roots: Vec<(String, String, Entity<TextareaState>)>,
    snapshots: Vec<crate::pokemon::storage::Snapshot>,
    show_snapshots: bool,
    _subs: Vec<Subscription>,
}

const DEFAULT_UI: &str = "Default (Inter)";
const DEFAULT_MONO: &str = "Default (JetBrains Mono NL)";

impl Settings {
    pub(crate) fn new(
        shark: WeakEntity<Shark>,
        state: &State,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let a = &state.appearance;
        let mut fonts: Vec<String> = cx
            .text_system()
            .all_font_names()
            .into_iter()
            .filter(|f| !f.starts_with('.'))
            .collect();
        fonts.sort_by_key(|f| f.to_lowercase());
        fonts.dedup();
        let configs = theme_configs();
        let swatches = |suffix: &str| {
            (THEMES.iter())
                .map(|(f, _)| {
                    let name = format!("{f} {suffix}");
                    Swatch::new(configs.iter().find(|t| t.name.as_ref() == name).unwrap())
                })
                .collect()
        };
        let mut choice = |items: Vec<SharedString>, current: &str, searchable: bool| {
            let ix = items
                .iter()
                .position(|i| i.as_ref() == current)
                .unwrap_or(0);
            cx.new(|cx| {
                SelectState::new(
                    SearchableVec::new(items),
                    Some(IndexPath::new(ix)),
                    window,
                    cx,
                )
                .searchable(searchable)
            })
        };
        let font_items = |default: &str| {
            std::iter::once(default.to_string())
                .chain(fonts.iter().cloned())
                .map(SharedString::from)
                .collect::<Vec<_>>()
        };
        let ui_font = choice(
            font_items(DEFAULT_UI),
            a.ui_font.as_deref().unwrap_or(DEFAULT_UI),
            true,
        );
        let mono_font = choice(
            font_items(DEFAULT_MONO),
            a.mono_font.as_deref().unwrap_or(DEFAULT_MONO),
            true,
        );

        let mut subs = vec![];
        let on_pick = |set: fn(&mut Appearance, Option<String>)| {
            move |this: &mut Self,
                  _: &Choice,
                  ev: &SelectEvent<SearchableVec<SharedString>>,
                  _: &mut Window,
                  cx: &mut Context<Self>| {
                let SelectEvent::Confirm(Some(v)) = ev else {
                    return;
                };
                let v = Some(v.to_string()).filter(|v| !v.starts_with("Default ("));
                this.appearance(cx, |a| set(a, v));
            }
        };
        subs.push(cx.subscribe_in(&ui_font, window, on_pick(|a, v| a.ui_font = v)));
        subs.push(cx.subscribe_in(&mono_font, window, on_pick(|a, v| a.mono_font = v)));

        let prompts = PROMPTS
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let text = prompts::get(p.id);
                // ponytail: auto_grow counts newlines, not wrapped lines; ~60 mono chars fit a row,
                // plus one spare so the last line is never cut.
                let rows = (text.len() / 60 + 2).clamp(3, 12);
                let ta = cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .auto_grow(rows, 14)
                        .default_value(text)
                });
                subs.push(cx.subscribe_in(&ta, window, move |this, ta, ev, _, cx| {
                    if let InputEvent::Change = ev {
                        let text = ta.read(cx).value().to_string();
                        this.set_prompt(i, Some(text), cx);
                    }
                }));
                ta
            })
            .collect();

        let roots = crate::usage::providers::builtins()
            .handles()
            .filter(|provider| provider.source().supports_custom_roots())
            .map(|provider| {
                let value = state
                    .usage_roots
                    .get(provider.id())
                    .cloned()
                    .unwrap_or_default();
                let input = cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .auto_grow(2, 6)
                        .default_value(value)
                });
                subs.push(cx.subscribe_in(&input, window, |_, _, _, _, cx| cx.notify()));
                (
                    provider.id().to_string(),
                    provider.name().to_string(),
                    input,
                )
            })
            .collect();
        Settings {
            shark,
            tab: 0,
            focus: cx.focus_handle(),
            light: swatches("Light"),
            dark: swatches("Dark"),
            ui_font,
            mono_font,
            prompts,
            roots,
            snapshots: State::pokemon_snapshots().list(),
            show_snapshots: false,
            _subs: subs,
        }
    }

    fn with_state<R>(&self, cx: &mut App, f: impl FnOnce(&mut State) -> R) -> Option<R> {
        self.shark
            .update(cx, |s, cx| {
                let r = f(&mut s.state);
                let _ = s.state.save();
                cx.notify();
                r
            })
            .ok()
    }

    pub(crate) fn appearance(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Appearance)) {
        self.shark.update(cx, |s, cx| s.set_appearance(cx, f)).ok();
        cx.notify();
    }

    fn set_stay_awake(&mut self, on: bool, cx: &mut Context<Self>) {
        self.shark
            .update(cx, |s, cx| {
                s.state.stay_awake = on;
                let _ = s.state.save();
                s.update_awake(cx);
            })
            .ok();
        cx.notify();
    }

    pub(crate) fn set_vim(&mut self, on: bool, cx: &mut Context<Self>) {
        self.shark.update(cx, |s, cx| s.set_vim(on, cx)).ok();
        cx.notify();
    }

    pub(crate) fn set_pokemon(&mut self, on: bool, cx: &mut Context<Self>) {
        self.shark
            .update(cx, |s, cx| s.set_pokemon_enabled(on, cx))
            .ok();
        cx.notify();
    }

    fn snapshot_pokemon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shark) = self.shark.upgrade() else {
            return;
        };
        match State::pokemon_snapshots()
            .create(&shark.read(cx).state.pokemon, crate::usage::now() as u64)
        {
            Ok(_) => {
                self.snapshots = State::pokemon_snapshots().list();
                self.show_snapshots = true;
            }
            Err(error) => crate::ui::notify_err(window, cx, error.to_string()),
        }
        cx.notify();
    }

    fn export_pokemon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shark) = self.shark.upgrade() else {
            return;
        };
        let state = shark.read(cx).state.pokemon.clone();
        let name = crate::pokemon::storage::suggested_file_name(crate::usage::now() as u64);
        let path = cx.prompt_for_new_path(&crate::state::home(), Some(&name));
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(path))) = path.await else {
                return;
            };
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::pokemon::storage::encode(&state, crate::usage::now() as u64)
                        .and_then(|bytes| crate::state::write(&path, &bytes).map_err(Into::into))
                })
                .await;
            if let Err(error) = result {
                let _ =
                    cx.update(|window, cx| crate::ui::notify_err(window, cx, error.to_string()));
            }
        })
        .detach();
    }

    fn import_pokemon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import Pokémon save".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let result = cx
                .background_executor()
                .spawn(async move { crate::pokemon::storage::read(&path) })
                .await;
            let _ = this.update_in(cx, |s, window, cx| match result {
                Ok(envelope) => s.confirm_pokemon_restore(envelope.state, window, cx),
                Err(error) => crate::ui::notify_err(window, cx, error.to_string()),
            });
        })
        .detach();
    }

    fn restore_pokemon_snapshot(
        &mut self,
        path: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match crate::pokemon::storage::read_snapshot(path) {
            Ok(envelope) => self.confirm_pokemon_restore(envelope.state, window, cx),
            Err(error) => crate::ui::notify_err(window, cx, error.to_string()),
        }
    }

    pub(crate) fn confirm_pokemon_restore(
        &mut self,
        imported: crate::pokemon::Companion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity().downgrade();
        let summary = format!(
            "Replace Pokémon progress with {} collected companions and {} earned tokens? A snapshot of your current progress will be saved first.",
            imported.collection.len(),
            imported.lifetime_tokens
        );
        window.open_dialog(cx, move |d, _, _| {
            let this = this.clone();
            let imported = imported.clone();
            // Return dispatches Confirm, which only closes this dialog. Replacing progress
            // requires activating the explicitly labelled button; Escape also cancels.
            dialog(d)
                .title("Restore Pokémon save")
                .w(px(520.))
                .on_ok(|_, _, _| true)
                .child(summary.clone())
                .footer(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("pokemon-import-cancel")
                                .label("Cancel")
                                .on_click(|_, w, cx| w.close_dialog(cx)),
                        )
                        .child(
                            Button::new("pokemon-import-replace")
                                .danger()
                                .label("Replace progress")
                                .on_click(move |_, window, cx| {
                                    this.update(cx, |s, cx| {
                                        let Some(shark) = s.shark.upgrade() else {
                                            return;
                                        };
                                        let current = shark.read(cx).state.pokemon.clone();
                                        match State::pokemon_snapshots()
                                            .create(&current, crate::usage::now() as u64)
                                        {
                                            Ok(_) => {
                                                let rebased = crate::pokemon::storage::rebase(
                                                    imported.clone(),
                                                    &current,
                                                    current.baselines.clone(),
                                                );
                                                shark.update(cx, |shark, cx| {
                                                    shark.apply_pokemon_save(rebased, cx)
                                                });
                                                s.snapshots = State::pokemon_snapshots().list();
                                                s.show_snapshots = true;
                                                window.close_dialog(cx);
                                                cx.notify();
                                            }
                                            Err(error) => crate::ui::notify_err(
                                                window,
                                                cx,
                                                format!("Progress was not replaced: {error}"),
                                            ),
                                        }
                                    })
                                    .ok();
                                }),
                        ),
                )
        });
    }

    /// `None`, blank or the default text drops the override.
    pub(crate) fn set_prompt(&mut self, i: usize, text: Option<String>, cx: &mut Context<Self>) {
        let p = &PROMPTS[i];
        let text = text.filter(|t| !t.trim().is_empty() && t != p.default);
        self.with_state(cx, |s| {
            match text {
                Some(t) => s.prompts.insert(p.id.into(), t),
                None => s.prompts.remove(p.id),
            };
            prompts::set_overrides(&s.prompts);
        });
        cx.notify();
    }

    pub(crate) fn reset_prompt(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.prompts[i].update(cx, |t, cx| t.set_value(PROMPTS[i].default, window, cx));
        self.set_prompt(i, None, cx);
    }

    fn saved_appearance(&self, cx: &App) -> Appearance {
        self.shark
            .upgrade()
            .map(|s| s.read(cx).state.appearance.clone())
            .unwrap_or_default()
    }

    /// A scrolling row of preview cards, one per family; clicking one picks it.
    fn theme_strip(
        &self,
        dark: bool,
        current: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let (id, swatches) = if dark {
            ("theme-dark", &self.dark)
        } else {
            ("theme-light", &self.light)
        };
        // `apply` shows Linear for an empty or unknown family, so mark that one.
        let current = (THEMES.iter().map(|(f, _)| *f))
            .find(|f| *f == current)
            .unwrap_or("Linear");
        let cards = (THEMES.iter().zip(swatches).enumerate()).map(|(i, ((family, _), s))| {
            let on = *family == current;
            let name = div()
                .text_xs()
                .text_color(if on {
                    theme.foreground
                } else {
                    theme.muted_foreground
                })
                .child(*family);
            Button::new((id, i))
                .text()
                .w(px(120.))
                .flex_none()
                .child(
                    v_flex()
                        .w_full()
                        .gap_1()
                        .child(preview(theme, s, on))
                        .child(name),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    let family = family.to_string();
                    this.appearance(cx, |a| {
                        *if dark { &mut a.dark } else { &mut a.light } = family
                    })
                }))
        });
        h_flex()
            .id(id)
            .w_full()
            .gap_2()
            // Room for the focus ring, which sits outside the card and the strip clips.
            .p_1()
            .overflow_x_scroll()
            // Wheel and trackpad move only their own axis, so the page and strips don't drag each other.
            .restrict_scroll_to_axis()
            .children(cards)
    }

    fn render_general(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (awake, vim, pokemon) = (self.shark.upgrade())
            .map(|s| {
                let state = &s.read(cx).state;
                (state.stay_awake, state.vim, !state.pokemon_disabled)
            })
            .unwrap_or_default();
        let this = cx.entity().downgrade();
        let theme = cx.theme();
        card(theme).w_full().min_w_0()
            .child(row(
                theme,
                false,
                "Stay awake while agents run",
                "The Mac doesn't idle-sleep mid-turn; the display may still turn off",
                Switch::new("stay-awake").checked(awake).on_click({
                    let this = this.clone();
                    move |&v, _, cx| {
                        this.update(cx, |s, cx| s.set_stay_awake(v, cx)).ok();
                    }
                }),
            ))
            .child(row(
                theme,
                true,
                "Vim mode",
                "Vim keys in the file editor, the composer and lists (ctrl-cmd-v)",
                Switch::new("vim").checked(vim).on_click({
                    let this = this.clone();
                    move |&v, _, cx| {
                        this.update(cx, |s, cx| s.set_vim(v, cx)).ok();
                    }
                }),
            ))
            .child(row(
                theme,
                true,
                "Pokémon companion",
                "Raise Pokémon with Claude Code and Codex tokens. Turning this off keeps your collection and pauses growth.",
                v_flex().gap_1().items_start()
                    .child(Switch::new("pokemon-enabled").checked(pokemon).on_click(move |&v, _, cx| {
                        this.update(cx, |s, cx| s.set_pokemon(v, cx)).ok();
                    }))
                    .child(Button::new("pokemon-choose-navigation").ghost().xsmall().label("Choose navigation Pokémon")
                        .on_click(cx.listener(|s,_,window,cx| {
                            window.close_dialog(cx);
                            s.shark.update(cx,|shark,cx|shark.show_pokemon_dex(window,cx)).ok();
                        }))),
            ))
            .child(row(theme,true,"Pokémon animation","Balance animation smoothness and battery use. Low Power Mode always limits the frame rate.",
                Button::new("pokemon-animation-quality").small()
                    .label(self.shark.upgrade().map(|shark|shark.read(cx).state.pokemon_animation_quality.name()).unwrap_or("Power saver"))
                    .on_click(cx.listener(|s,_,_,cx| {
                        s.shark.update(cx,|shark,cx| {shark.state.pokemon_animation_quality=shark.state.pokemon_animation_quality.next();shark.refresh_pokemon_animation(cx);let _=shark.state.save();cx.notify();}).ok();cx.notify();
                    }))))
            .child(stacked(theme, "Pokémon saves", "Snapshots keep your collection recoverable. Export a Shark save to move it to another Mac.",
                v_flex().gap_2()
                    .child(h_flex().gap_2()
                        .child(Button::new("pokemon-snapshot").small().label("Create snapshot").on_click(cx.listener(|s, _, w, cx| s.snapshot_pokemon(w, cx))))
                        .child(Button::new("pokemon-export").small().label("Export").on_click(cx.listener(|s, _, w, cx| s.export_pokemon(w, cx))))
                        .child(Button::new("pokemon-import").small().label("Import").on_click(cx.listener(|s, _, w, cx| s.import_pokemon(w, cx)))))
                    .child(Button::new("pokemon-snapshot-history").ghost().small()
                        .label(if self.show_snapshots { "Hide snapshot history" } else { "Show snapshot history" })
                        .on_click(cx.listener(|s, _, _, cx| { s.show_snapshots = !s.show_snapshots; cx.notify(); })))
                    .when(self.show_snapshots && self.snapshots.is_empty(), |d| d.child(div().id("pokemon-snapshots-empty").text_sm().child("No snapshots yet").test_support()))
                    .when(self.show_snapshots, |d| d.children(self.snapshots.iter().enumerate().map(|(i, snapshot)| {
                        let path = snapshot.path.clone();
                        let formatter = objc2_foundation::NSDateFormatter::new();
                        formatter.setDateFormat(Some(&objc2_foundation::NSString::from_str("yyyy-MM-dd HH:mm")));
                        let date = formatter.stringFromDate(&objc2_foundation::NSDate::dateWithTimeIntervalSince1970(snapshot.exported_at as f64));
                        Button::new(("pokemon-restore", i)).ghost().small()
                            .label(format!("{} · {} collected · {} tokens", date, snapshot.collection_count, snapshot.lifetime_tokens))
                            .on_click(cx.listener(move |s, _, w, cx| s.restore_pokemon_snapshot(&path, w, cx)))
                    }))),
            ))
    }

    pub(crate) fn save_roots(&mut self, index: usize, cx: &mut Context<Self>) {
        let (id, _, input) = &self.roots[index];
        let raw = input.read(cx).value().to_string();
        self.shark
            .update(cx, |shark, cx| shark.set_usage_roots(id, &raw, cx))
            .ok();
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn roots_input(&self, id: &str) -> Entity<TextareaState> {
        self.roots
            .iter()
            .find(|(key, _, _)| key == id)
            .unwrap()
            .2
            .clone()
    }

    fn render_roots(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().w_full().min_w_0().gap_4()
            .child("Shark finds the standard log folders automatically. Add folders for other local installations here; each tool keeps its own list.")
            .child(div().text_sm().text_color(cx.theme().muted_foreground)
                .child("Use one path per line or separate paths with commas. ~ and directory wildcards are supported. Save an empty list to use only the standard folders."))
            .children(self.roots.iter().enumerate().map(|(index, (id, name, input))| {
                let saved = self.shark.upgrade().map(|shark| shark.read(cx).state.usage_roots.get(id).cloned().unwrap_or_default()).unwrap_or_default();
                let changed = input.read(cx).value().trim() != saved.trim();
                v_flex().w_full().min_w_0().gap_2()
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(name.clone()))
                    .child(Textarea::new(input))
                    .child(Button::new(SharedString::from(format!("token-roots-save-{id}")))
                        .small().label("Save folders").disabled(!changed)
                        .on_click(cx.listener(move |this, _, _, cx| this.save_roots(index, cx))))
            }))
    }

    fn render_license(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .gap_4()
            .child(section(theme, "Shark · GPL-3.0"))
            .child("Copyright Kasper Munch Jensen. Shark is free software distributed under the GNU General Public License, version 3.")
            .child(Button::new("license-shark").ghost().small().role(Role::Link).label("Read Shark’s full license").on_click(|_, _, cx| cx.open_url("https://github.com/KaffeDiem/shark/blob/master/LICENSE")))
            .child(section(theme, "PokeTokenBar · MIT"))
            .child("The Pokémon companion is a Rust adaptation of PokeTokenBar by chattymin and contributors.")
            .child(Button::new("license-poketokenbar").ghost().small().role(Role::Link).label("PokeTokenBar source").on_click(|_, _, cx| cx.open_url("https://github.com/chattymin/PokeTokenBar")))
            .child(div().text_xs().child(include_str!("../licenses/PokeTokenBar-MIT.txt")))
            .child(section(theme, "Pokémon data and artwork"))
            .child("Pokémon names, characters and artwork belong to their respective owners. Shark is not affiliated with or endorsed by Nintendo, Game Freak, Creatures Inc. or The Pokémon Company. The MIT license applies to the adapted source code; it grants no rights to Pokémon artwork or trademarks.")
            .child("Species data and sprites are downloaded from PokéAPI and its sprite repository, cached locally, and are not bundled with Shark. No usage logs, prompts or project paths are sent to these services.")
            .child(Button::new("license-pokeapi").ghost().small().role(Role::Link).label("PokéAPI").on_click(|_, _, cx| cx.open_url("https://pokeapi.co/")))
    }

    fn render_appearance(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let saved = self.saved_appearance(cx);
        let mode = saved.mode;
        let light = self.theme_strip(false, &saved.light, cx);
        let dark = self.theme_strip(true, &saved.dark, cx);
        let theme = cx.theme();
        let mode_group = h_flex().gap_1().children(mode_pills(mode).map(|(m, pill)| {
            pill.on_click(cx.listener(move |this, _, _, cx| this.appearance(cx, |a| a.mode = m)))
        }));
        let sample = div()
            .px_3()
            .py_2()
            .rounded(theme.radius)
            .bg(theme.muted)
            .font_family(theme.mono_font_family.clone())
            .text_size(theme.mono_font_size)
            .text_color(theme.foreground)
            .child("let id = 0x1Il0O; // git push -u origin main");
        v_flex()
            .gap_3()
            .child(section(theme, "Theme"))
            .child(
                card(theme)
                    .child(row(
                        theme,
                        false,
                        "Mode",
                        "System follows macOS",
                        mode_group,
                    ))
                    .child(stacked(theme, "Light theme", "Used in light mode", light))
                    .child(stacked(theme, "Dark theme", "Used in dark mode", dark)),
            )
            .child(section(theme, "Fonts"))
            .child(
                card(theme)
                    .child(row(
                        theme,
                        false,
                        "Interface font",
                        "Sidebar, transcript and menus",
                        Select::new(&self.ui_font).small().menu_max_h(px(320.)),
                    ))
                    .child(row(
                        theme,
                        true,
                        "Code font",
                        "Code, diffs and tool output",
                        Select::new(&self.mono_font).small().menu_max_h(px(320.)),
                    ))
                    .child(
                        div()
                            .p_3()
                            .border_t_1()
                            .border_color(theme.border)
                            .child(sample),
                    ),
            )
    }

    fn render_prompts(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (mono, mono_size, hint) = (
            theme.mono_font_family.clone(),
            theme.mono_font_size,
            faint(theme),
        );
        let (border, radius, muted_fg, secondary) = (
            theme.border,
            theme.radius,
            theme.muted_foreground,
            theme.secondary,
        );
        let edited: Vec<bool> = PROMPTS
            .iter()
            .map(|p| prompts::get(p.id) != p.default)
            .collect();
        v_flex()
            .gap_4()
            .children(PROMPTS.iter().enumerate().map(|(i, p)| {
                v_flex()
                    .gap_1p5()
                    .child(
                        h_flex()
                            .h(px(24.))
                            .gap_2()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(p.title),
                            )
                            .when(edited[i], |r| {
                                r.child(div().text_xs().text_color(muted_fg).child("Edited"))
                            })
                            .child(div().flex_1())
                            .when(edited[i], |r| {
                                r.child(
                                    Button::new(("reset", i))
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::RotateCcw)
                                        .label("Reset to default")
                                        .on_click(cx.listener(move |this, _, w, cx| {
                                            this.reset_prompt(i, w, cx)
                                        })),
                                )
                            }),
                    )
                    .child(
                        div()
                            .rounded(radius)
                            .border_1()
                            .border_color(border)
                            .bg(secondary)
                            .font_family(mono.clone())
                            .text_size(mono_size)
                            .child(Textarea::new(&self.prompts[i]).appearance(false)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(hint)
                            .child(format!("{{repos}}: {}", p.repos)),
                    )
            }))
    }
}

/// Settings pages: nav icon, then title and the one line under it.
const PAGES: &[(IconName, (&str, &str))] = &[
    (
        IconName::Settings,
        ("General", "How Shark behaves while it works"),
    ),
    (
        IconName::Palette,
        ("Appearance", "Themes and fonts, applied as you pick them"),
    ),
    (
        IconName::MessageSquareText,
        (
            "Prompts",
            "Sent to the agent by the git button; {repos} and {branch} are filled in when the step runs",
        ),
    ),
    (
        IconName::BookOpen,
        ("LICENSE", "Open-source licenses and Pokémon attribution"),
    ),
    (
        IconName::Folder,
        (
            "Token sources",
            "Extra local log folders for Pokémon growth and token totals",
        ),
    ),
];

/// A tiny mock of the app in a theme's colors: sidebar, text lines and a primary button.
fn preview(theme: &Theme, s: &Swatch, on: bool) -> Div {
    let bar = |color: Hsla, w: f32| div().h(px(4.)).w(px(w)).rounded_full().bg(color);
    h_flex()
        .h(px(64.))
        .w_full()
        .overflow_hidden()
        .rounded(theme.radius)
        .border_2()
        .border_color(if on { theme.primary } else { theme.border })
        .bg(s.bg)
        .child(
            div()
                .w(px(28.))
                .h_full()
                .border_r_1()
                .border_color(s.border)
                .bg(s.sidebar),
        )
        .child(
            v_flex()
                .flex_1()
                .h_full()
                .p_2()
                .gap_1p5()
                .child(bar(s.fg, 44.))
                .child(bar(s.muted, 60.))
                .child(bar(s.muted, 36.))
                .child(
                    div()
                        .mt_auto()
                        .h(px(8.))
                        .w(px(24.))
                        .rounded_sm()
                        .bg(s.primary),
                ),
        )
}

/// A separated row with the label on top and a full-width control beneath.
fn stacked(
    theme: &Theme,
    label: &'static str,
    desc: &'static str,
    control: impl IntoElement,
) -> impl IntoElement {
    v_flex()
        .min_w_0()
        .p_3()
        .gap_2()
        .border_t_1()
        .border_color(theme.border)
        .child(caption(theme, label, desc))
        .child(control)
}

impl Render for Settings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.tab {
            0 => self.render_general(cx).into_any_element(),
            1 => self.render_appearance(cx).into_any_element(),
            2 => self.render_prompts(cx).into_any_element(),
            3 => self.render_license(cx).into_any_element(),
            _ => self.render_roots(cx).into_any_element(),
        };
        let (title, desc) = PAGES[self.tab].1;
        let theme = cx.theme();
        // A page list on the left, the page on the right (FLORA, Teams, Braintrust on Mobbin).
        h_flex()
            .key_context("Settings")
            .track_focus(&self.focus)
            .items_start()
            .gap_6()
            .on_action(cx.listener(|this, _: &SwitchField, _, cx| {
                this.tab = (this.tab + 1) % PAGES.len();
                cx.notify();
            }))
            .child(v_flex().w(px(168.)).flex_none().gap_0p5().children(
                PAGES.iter().enumerate().map(|(i, (icon, (label, _)))| {
                    let on = i == self.tab;
                    // Rows, not buttons: a Button centres its content, so the icons wouldn't line up.
                    h_flex()
                        .id(*label)
                        .h(px(28.))
                        .px_2()
                        .gap_2()
                        .rounded(theme.radius)
                        .text_sm()
                        .text_color(if on {
                            theme.foreground
                        } else {
                            theme.muted_foreground
                        })
                        .when(on, |d| d.bg(theme.accent))
                        .when(!on, |d| d.hover(|d| d.bg(theme.secondary_hover)))
                        .child(gpui_kit::component::Icon::new(*icon).with_size(px(14.)))
                        .child(*label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.tab = i;
                            cx.notify();
                        }))
                        .test_support()
                }),
            ))
            .child(
                // A fixed height, so switching pages doesn't move the dialog; long pages scroll.
                v_flex()
                    .id("settings-page")
                    .flex_1()
                    .min_w_0()
                    .h(px(520.))
                    .overflow_y_scroll()
                    .restrict_scroll_to_axis()
                    .gap_4()
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_base()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(desc),
                            ),
                    )
                    .child(body)
                    .test_support(),
            )
    }
}
