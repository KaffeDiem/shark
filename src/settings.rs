//! Settings dialog (cmd-,): appearance (mode, light and dark theme, fonts) and the git button's
//! agent prompts. Every change applies and saves at once; there is no Save button.
use crate::app::{Shark, faint, inter_features};
use crate::prompts::{self, PROMPTS};
use crate::state::{Appearance, Mode, State};
use crate::{SwitchField, THEMES};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_kit::component::{
    ActiveTheme, IndexPath, Selectable as _, Sizable as _, Theme, ThemeMode, ThemeRegistry,
    WindowExt, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::rc::Rc;

/// Installs the saved light and dark themes, with the font overrides baked in so a
/// mode switch (which reloads the config) keeps them, then picks the mode.
pub fn apply(a: &Appearance, cx: &mut App) {
    let registry = ThemeRegistry::global(cx);
    let stock = [
        registry.default_dark_theme().highlight.clone(),
        registry.default_light_theme().highlight.clone(),
    ];
    let configs = crate::theme_configs();
    let pick = |family: &str, mode: ThemeMode| {
        let suffix = if mode.is_dark() { "Dark" } else { "Light" };
        let mut t = [family, "Linear"]
            .iter()
            .find_map(|f| {
                let name = format!("{f} {suffix}");
                configs.iter().find(|t| t.name.as_ref() == name)
            })
            .expect("Linear theme")
            .clone();
        // Syntax colors aren't restyled; borrow the stock ones of the same mode.
        t.highlight = stock[!mode.is_dark() as usize].clone();
        if let Some(f) = &a.ui_font {
            t.font_family = Some(f.clone().into());
        }
        if let Some(f) = &a.mono_font {
            t.mono_font_family = Some(f.clone().into());
        }
        Rc::new(t)
    };
    let (dark, light) = (
        pick(&a.dark, ThemeMode::Dark),
        pick(&a.light, ThemeMode::Light),
    );
    Theme::update(cx, |t| {
        t.dark_theme = dark;
        t.light_theme = light;
    });
    let mode = match a.mode {
        Mode::System => cx.window_appearance().into(),
        Mode::Light => ThemeMode::Light,
        Mode::Dark => ThemeMode::Dark,
    };
    Theme::change(mode, None, cx);
}

pub fn open(state: &State, window: &mut Window, cx: &mut Context<Shark>) {
    let shark = cx.entity().downgrade();
    let view = cx.new(|cx| Settings::new(shark, state, window, cx));
    let focus = view.read(cx).focus.clone();
    window.open_dialog(cx, move |d, _, _| {
        d.font_features(inter_features())
            .title("Settings")
            .w(px(640.))
            .child(view.clone())
    });
    focus.focus(window, cx);
}

type Choice = Entity<SelectState<SearchableVec<SharedString>>>;

pub struct Settings {
    shark: WeakEntity<Shark>,
    tab: usize,
    focus: FocusHandle,
    light: Choice,
    dark: Choice,
    ui_font: Choice,
    mono_font: Choice,
    prompts: Vec<Entity<TextareaState>>,
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
        let families: Vec<SharedString> = THEMES.iter().map(|(n, _)| (*n).into()).collect();
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
        let light = choice(families.clone(), &a.light, false);
        let dark = choice(families, &a.dark, false);
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
        subs.push(cx.subscribe_in(
            &light,
            window,
            on_pick(|a, v| a.light = v.unwrap_or_default()),
        ));
        subs.push(cx.subscribe_in(
            &dark,
            window,
            on_pick(|a, v| a.dark = v.unwrap_or_default()),
        ));
        subs.push(cx.subscribe_in(&ui_font, window, on_pick(|a, v| a.ui_font = v)));
        subs.push(cx.subscribe_in(&mono_font, window, on_pick(|a, v| a.mono_font = v)));

        let prompts = PROMPTS
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let text = prompts::get(p.id);
                // ponytail: auto_grow counts newlines, not wrapped lines; ~70 mono chars fit a row.
                let rows = (text.len() / 70 + 1).clamp(3, 10);
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

        Settings {
            shark,
            tab: 0,
            focus: cx.focus_handle(),
            light,
            dark,
            ui_font,
            mono_font,
            prompts,
            _subs: subs,
        }
    }

    fn with_state<R>(&self, cx: &mut App, f: impl FnOnce(&mut State) -> R) -> Option<R> {
        self.shark
            .update(cx, |s, _| {
                let r = f(&mut s.state);
                s.state.save();
                r
            })
            .ok()
    }

    pub(crate) fn appearance(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Appearance)) {
        if let Some(a) = self.with_state(cx, |s| {
            f(&mut s.appearance);
            s.appearance.clone()
        }) {
            apply(&a, cx);
        }
        cx.notify();
    }

    /// `None` or the default text drops the override.
    pub(crate) fn set_prompt(&mut self, i: usize, text: Option<String>, cx: &mut Context<Self>) {
        let p = &PROMPTS[i];
        let text = text.filter(|t| t != p.default);
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

    fn mode(&self, cx: &App) -> Mode {
        self.shark
            .upgrade()
            .map(|s| s.read(cx).state.appearance.mode)
            .unwrap_or_default()
    }

    fn render_appearance(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self.mode(cx);
        let theme = cx.theme();
        let modes = [
            (Mode::System, "System", IconName::Monitor),
            (Mode::Light, "Light", IconName::Sun),
            (Mode::Dark, "Dark", IconName::Moon),
        ];
        let mode_group = h_flex()
            .gap_1()
            .children(modes.iter().map(|(m, label, icon)| {
                let m = *m;
                pill(label, label, Some(*icon), m == mode).on_click(
                    cx.listener(move |this, _, _, cx| this.appearance(cx, |a| a.mode = m)),
                )
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
                    .child(row(
                        theme,
                        true,
                        "Light theme",
                        "Used in light mode",
                        Select::new(&self.light).small(),
                    ))
                    .child(row(
                        theme,
                        true,
                        "Dark theme",
                        "Used in dark mode",
                        Select::new(&self.dark).small(),
                    )),
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
            .child(
                div()
                    .text_xs()
                    .text_color(muted_fg)
                    .child("Sent to the agent by the git button. {repos} and {branch} are filled in when the step runs."),
            )
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

/// A ghost button that shows its selection, so tabs and mode are tab stops with enter/space.
fn pill(id: &'static str, label: &'static str, icon: Option<IconName>, active: bool) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .label(label)
        .when_some(icon, |b, i| b.icon(i))
        .selected(active)
}

fn section(theme: &Theme, label: &'static str) -> impl IntoElement {
    div()
        .text_xs()
        .text_color(theme.muted_foreground)
        .font_weight(FontWeight::MEDIUM)
        .child(label)
}

fn card(theme: &Theme) -> Div {
    v_flex()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
        .bg(theme.secondary)
}

/// Label and description on the left, the control on the right (Linear's settings rows).
fn row(
    theme: &Theme,
    sep: bool,
    label: &'static str,
    desc: &'static str,
    control: impl IntoElement,
) -> impl IntoElement {
    h_flex()
        .min_h(px(52.))
        .px_3()
        .gap_3()
        .when(sep, |r| r.border_t_1().border_color(theme.border))
        .child(
            v_flex().flex_1().child(div().text_sm().child(label)).child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(desc),
            ),
        )
        .child(div().w(px(240.)).flex().justify_end().child(control))
}

impl Render for Settings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.tab {
            0 => self.render_appearance(cx).into_any_element(),
            _ => self.render_prompts(cx).into_any_element(),
        };
        v_flex()
            .key_context("Settings")
            .track_focus(&self.focus)
            .gap_4()
            .on_action(cx.listener(|this, _: &SwitchField, _, cx| {
                this.tab = 1 - this.tab;
                cx.notify();
            }))
            .child(
                h_flex()
                    .gap_1()
                    .children(
                        ["Appearance", "Prompts"]
                            .iter()
                            .enumerate()
                            .map(|(i, label)| {
                                pill(label, label, None, i == self.tab).on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.tab = i;
                                        cx.notify();
                                    },
                                ))
                            }),
                    ),
            )
            .child(body)
    }
}
