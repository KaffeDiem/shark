//! One picker for agent symbols, emoji and locally cropped photos.
use super::*;
use crate::{agent_icon, state::AgentIcon};
use gpui_kit::component::FocusTrapElement as _;
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use std::sync::Arc;

#[path = "icon_picker_symbols.rs"]
mod symbols;
use symbols::SYMBOLS;

const COLUMNS: usize = 9;
const TINTS: &[&str] = &[
    "neutral", "red", "orange", "yellow", "green", "blue", "purple", "pink",
];

pub(super) fn symbol(name: &str) -> IconName {
    // Preserve legacy names even if they are outside the curated picker set.
    IconName::ALL
        .iter()
        .copied()
        .find(|icon| {
            let path = icon.path();
            path.strip_prefix("icons/")
                .and_then(|p| p.strip_suffix(".svg"))
                == Some(name)
        })
        .unwrap_or(IconName::Bot)
}

pub(super) fn tint(token: &str, theme: &Theme) -> Option<Hsla> {
    let color = match token {
        "neutral" => return Some(theme.sidebar_foreground),
        "red" => 0xd47878,
        "orange" => 0xc88b61,
        "yellow" => 0xb5a35d,
        "green" => 0x7aab83,
        "blue" => 0x799fc9,
        "purple" => 0xa58ac5,
        "pink" => 0xc486aa,
        _ => return None,
    };
    Some(rgb(color).into())
}

fn symbol_matches(name: &str, query: &str) -> bool {
    let tags = match name {
        "bot" => "robot agent assistant ai",
        "terminal" | "code" | "code-xml" | "braces" => "developer programming computer software",
        "palette" | "paintbrush" | "brush" | "pen-tool" => "design art creative",
        "flask-conical" | "microscope" | "atom" => "research science experiment",
        "brain" | "lightbulb" => "ideas think intelligence",
        "shield" | "lock" | "fingerprint" => "security privacy safe",
        "music" | "guitar" | "headphones" => "audio sound musician",
        "users" | "user" | "user-round" => "person people team",
        "star" => "favorite favourite featured",
        _ => "",
    };
    query
        .split_whitespace()
        .all(|word| name.contains(word) || tags.contains(word))
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Icons,
    Emoji,
    Image,
}

#[derive(Clone)]
enum Choice {
    Symbol(&'static str, IconName),
    Emoji(&'static emojis::Emoji),
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Preferences {
    frequent: Vec<(String, u32)>,
    tone: usize,
}

impl Preferences {
    fn load() -> Self {
        std::fs::read(crate::state::support_dir().join("agent-icon-picker.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
    fn save(&self) {
        if let Ok(bytes) = serde_json::to_vec(self) {
            let _ = crate::state::write(
                &crate::state::support_dir().join("agent-icon-picker.json"),
                &bytes,
            );
        }
    }
    fn record(&mut self, emoji: &emojis::Emoji) {
        if let Some(tone) = emoji.skin_tone() {
            self.tone = tone as usize;
        }
        if let Some((_, count)) = self.frequent.iter_mut().find(|(s, _)| s == emoji.as_str()) {
            *count = count.saturating_add(1);
        } else {
            self.frequent.push((emoji.as_str().into(), 1));
        }
        self.frequent.sort_by(|a, b| b.1.cmp(&a.1));
        self.frequent.truncate(36);
        self.save();
    }
}

pub(super) enum Event {
    Apply(AgentIcon),
    Dismiss,
}

pub(super) struct IconPicker {
    current: AgentIcon,
    slug: String,
    tab: Tab,
    search: Entity<InputState>,
    focus: FocusHandle,
    choices: Vec<Choice>,
    selected: usize,
    tint: Option<String>,
    preferences: Preferences,
    scroll: UniformListScrollHandle,
    image: Option<Arc<image::RgbaImage>>,
    preview: Option<Arc<RenderImage>>,
    center: [f32; 2],
    zoom: Entity<SliderState>,
    dragging: Option<Point<Pixels>>,
    dropping: Option<PathBuf>,
    busy: bool,
    generation: u64,
    message: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Event> for IconPicker {}

impl IconPicker {
    pub(super) fn new(
        current: AgentIcon,
        slug: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let tab = match current {
            AgentIcon::Symbol { .. } => Tab::Icons,
            AgentIcon::Emoji { .. } => Tab::Emoji,
            AgentIcon::Image { .. } => Tab::Image,
        };
        let tint = match &current {
            AgentIcon::Symbol { tint, .. } => tint.clone(),
            _ => None,
        };
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let zoom = cx.new(|_| {
            SliderState::new()
                .min(1.)
                .max(4.)
                .step(0.05)
                .default_value(1.)
        });
        let search_subscription = cx.subscribe(&search, |this, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.filter(cx);
                cx.notify();
            }
        });
        let zoom_subscription = cx.subscribe(&zoom, |this, _, event, cx| {
            if matches!(event, SliderEvent::Change(_)) {
                this.clamp_center(cx);
                cx.notify();
            }
        });
        let weak = cx.entity().downgrade();
        let keys = cx.intercept_keystrokes(move |event, w, cx| {
            weak.update(cx, |this, cx| {
                if !this.focus.contains_focused(w, cx)
                    && !this.search.read(cx).focus_handle(cx).is_focused(w)
                {
                    return;
                }
                if this.key(&event.keystroke, w, cx) {
                    cx.stop_propagation();
                }
            })
            .ok();
        });
        let mut picker = Self {
            current,
            slug,
            tab,
            search,
            focus: cx.focus_handle(),
            choices: vec![],
            selected: 0,
            tint,
            preferences: Preferences::load(),
            scroll: UniformListScrollHandle::new(),
            image: None,
            preview: None,
            center: [0.5, 0.5],
            zoom,
            dragging: None,
            dropping: None,
            busy: false,
            generation: 0,
            message: None,
            _subscriptions: vec![search_subscription, zoom_subscription, keys],
        };
        picker.filter(cx);
        if let AgentIcon::Image { file } = &picker.current {
            if let Some(path) = agent_icon::path(file) {
                picker.load_path(&path, false, window, cx);
            }
        }
        picker
    }

    pub(super) fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == Tab::Image {
            self.focus.focus(window, cx);
        } else {
            self.search.read(cx).focus_handle(cx).focus(window, cx);
        }
    }

    pub(super) fn cancel(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.busy = false;
        self.dragging = None;
        self.dropping = None;
        self.image = None;
        if let Some(image) = self.preview.take() {
            cx.drop_image(image, None);
        }
    }

    fn change_tab(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        self.generation += 1;
        self.busy = false;
        self.message = None;
        self.tab = tab;
        self.search
            .update(cx, |search, cx| search.set_value("", window, cx));
        self.filter(cx);
        self.focus(window, cx);
        cx.notify();
    }

    fn filter(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).value().trim().to_lowercase();
        self.choices = match self.tab {
            Tab::Icons => SYMBOLS
                .iter()
                .filter(|(name, _)| symbol_matches(name, &query))
                .map(|(n, i)| Choice::Symbol(n, *i))
                .collect(),
            Tab::Emoji => emojis::iter()
                .filter(|emoji| {
                    query.is_empty() || {
                        let name = emoji.name().to_lowercase();
                        query.split_whitespace().all(|q| {
                            name.contains(q)
                                || emoji.shortcodes().any(|s| s.contains(q))
                                || emoji.as_str() == q
                        })
                    }
                })
                .map(|emoji| Choice::Emoji(self.toned(emoji)))
                .collect(),
            Tab::Image => vec![],
        };
        self.selected = 0;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
    }

    fn toned(&self, emoji: &'static emojis::Emoji) -> &'static emojis::Emoji {
        emoji
            .skin_tones()
            .and_then(|mut tones| tones.nth(self.preferences.tone))
            .unwrap_or(emoji)
    }

    fn key(&mut self, key: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if key.modifiers.platform && matches!(key.key.as_str(), "v" | "d") {
            self.paste(window, cx);
            return true;
        }
        let search_focused = self.search.read(cx).focus_handle(cx).is_focused(window);
        let grid_focused = self.focus.is_focused(window);
        match key.key.as_str() {
            "escape" => {
                self.cancel(cx);
                cx.emit(Event::Dismiss);
            }
            "enter" if self.tab == Tab::Image && grid_focused => self.done(window, cx),
            "enter" if self.tab != Tab::Image && (grid_focused || search_focused) => {
                self.pick(self.selected, cx)
            }
            "left" | "right" | "up" | "down"
                if self.tab == Tab::Image
                    && self.focus.is_focused(window)
                    && self.image.is_some() =>
            {
                let (axis, step) = match key.key.as_str() {
                    "left" => (0, -0.025),
                    "right" => (0, 0.025),
                    "up" => (1, -0.025),
                    _ => (1, 0.025),
                };
                self.center[axis] += step / self.zoom.read(cx).value().start();
                self.clamp_center(cx);
                cx.notify();
            }
            "tab" if self.tab != Tab::Image && search_focused && !key.modifiers.shift => {
                self.focus.focus(window, cx);
            }
            "tab" if self.tab != Tab::Image && grid_focused && key.modifiers.shift => {
                self.search.read(cx).focus_handle(cx).focus(window, cx);
            }
            "left" | "right" | "up" | "down"
                if self.tab != Tab::Image
                    && (grid_focused || search_focused)
                    && !self.choices.is_empty() =>
            {
                let step = match key.key.as_str() {
                    "left" => -1,
                    "right" => 1,
                    "up" => -(COLUMNS as isize),
                    _ => COLUMNS as isize,
                };
                self.selected = self
                    .selected
                    .saturating_add_signed(step)
                    .min(self.choices.len() - 1);
                self.scroll
                    .scroll_to_item(self.selected / COLUMNS, ScrollStrategy::Nearest);
                cx.notify();
            }
            _ => return false,
        }
        true
    }

    fn pick(&mut self, index: usize, cx: &mut Context<Self>) {
        self.generation += 1;
        match self.choices.get(index).cloned() {
            Some(Choice::Symbol(name, _)) => cx.emit(Event::Apply(AgentIcon::Symbol {
                name: name.into(),
                tint: self.tint.clone(),
            })),
            Some(Choice::Emoji(emoji)) => self.pick_emoji(emoji, cx),
            None => {}
        }
    }

    fn pick_emoji(&mut self, emoji: &'static emojis::Emoji, cx: &mut Context<Self>) {
        self.generation += 1;
        self.preferences.record(emoji);
        cx.emit(Event::Apply(AgentIcon::Emoji {
            value: emoji.as_str().into(),
        }));
    }

    pub(super) fn paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel(cx);
        self.change_tab(Tab::Image, window, cx);
        if let Some(item) = cx.read_from_clipboard() {
            if let Some(image) = item.entries().iter().find_map(|e| match e {
                ClipboardEntry::Image(image) => Some(image),
                _ => None,
            }) {
                let bytes = image.bytes.clone();
                return self.load(move || agent_icon::decode(&bytes), false, window, cx);
            }
            if let Some(path) = item.entries().iter().find_map(|e| match e {
                ClipboardEntry::ExternalPaths(paths) => paths.paths().first(),
                _ => None,
            }) {
                return self.load_path(path, false, window, cx);
            }
        }
        self.message = Some("No image on the pasteboard".into());
        cx.notify();
    }

    pub(super) fn load_path(
        &mut self,
        path: &Path,
        skip_square: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = path.to_owned();
        self.load(
            move || agent_icon::decode_file(&path),
            skip_square,
            window,
            cx,
        );
    }

    fn load(
        &mut self,
        decode: impl FnOnce() -> Result<image::RgbaImage, String> + Send + 'static,
        skip_square: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tab = Tab::Image;
        self.busy = true;
        self.message = None;
        self.image = None;
        if let Some(image) = self.preview.take() {
            cx.drop_image(image, Some(window));
        }
        self.generation += 1;
        let generation = self.generation;
        self.focus.focus(window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    decode().map(|image| {
                        let mut preview = image::imageops::thumbnail(&image, 1024, 1024);
                        for pixel in preview.pixels_mut() {
                            pixel.0.swap(0, 2);
                        }
                        let preview =
                            Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(
                                preview
                            )]));
                        (Arc::new(image), preview)
                    })
                })
                .await;
            this.update_in(cx, |this, w, cx| {
                if generation != this.generation {
                    return;
                }
                this.busy = false;
                match result {
                    Ok((image, preview)) => {
                        let square = image.width() == image.height();
                        this.image = Some(image);
                        this.preview = Some(preview);
                        this.center = [0.5, 0.5];
                        this.zoom.update(cx, |zoom, cx| zoom.set_value(1., w, cx));
                        if skip_square && square {
                            this.done(w, cx);
                        }
                    }
                    Err(error) => this.message = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn choose(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose image".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                if let Some(path) = paths.first() {
                    this.update_in(cx, |this, w, cx| this.load_path(path, false, w, cx))
                        .ok();
                }
            }
        })
        .detach();
    }

    fn clamp_center(&mut self, cx: &App) {
        if let Some(image) = &self.image {
            let side =
                image.width().min(image.height()) as f32 / self.zoom.read(cx).value().start();
            for (center, size) in self.center.iter_mut().zip([image.width(), image.height()]) {
                let half = side / size as f32 / 2.;
                *center = center.clamp(half, 1. - half);
            }
        }
    }

    fn done(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(image) = self.image.clone() else {
            return;
        };
        let (center, zoom, generation) = (
            self.center,
            self.zoom.read(cx).value().start(),
            self.generation,
        );
        self.busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let cropped = cx
                .background_executor()
                .spawn(async move { agent_icon::crop_image(&image, center, zoom) })
                .await;
            this.update_in(cx, |this, _, cx| {
                if generation != this.generation {
                    return;
                }
                this.busy = false;
                // Only write after the crop is still current; cancelled work creates no orphan files.
                match agent_icon::save_image(&this.slug, &cropped) {
                    Ok(icon) => cx.emit(Event::Apply(icon)),
                    Err(error) => this.message = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn cell(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(choice) = self.choices.get(index) else {
            return div().size(px(32.)).into_any_element();
        };
        let theme = cx.theme();
        let (label, content) = match choice {
            Choice::Symbol(name, icon) => (
                name.to_string(),
                Icon::new(*icon)
                    .with_size(px(18.))
                    .text_color(
                        self.tint
                            .as_deref()
                            .and_then(|t| tint(t, theme))
                            .unwrap_or(theme.foreground),
                    )
                    .into_any_element(),
            ),
            Choice::Emoji(emoji) => (
                emoji.name().into(),
                div()
                    .font_family("Apple Color Emoji")
                    .text_size(px(22.))
                    .child(emoji.as_str())
                    .into_any_element(),
            ),
        };
        div()
            .id(("agent-icon-choice", index))
            .role(Role::ListBoxOption)
            .aria_label(label.clone())
            .aria_selected(index == self.selected)
            .size(px(32.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(5.))
            .cursor_pointer()
            .when(index == self.selected, |d| d.bg(theme.accent))
            .hover(|d| d.bg(theme.accent))
            .tooltip(move |w, cx| Tooltip::new(label.clone()).build(w, cx))
            .child(content)
            .on_click(cx.listener(move |this, _, _, cx| this.pick(index, cx)))
            .test_support()
            .into_any_element()
    }

    fn image_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let content = if let (Some(image), Some(preview)) = (&self.image, &self.preview) {
            let side =
                image.width().min(image.height()) as f32 / self.zoom.read(cx).value().start();
            let width = 120. * image.width() as f32 / side;
            let height = 120. * image.height() as f32 / side;
            v_flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .id("agent-icon-crop")
                        .size(px(120.))
                        .relative()
                        .rounded_full()
                        .overflow_hidden()
                        .cursor(CursorStyle::OpenHand)
                        .role(Role::Image)
                        .aria_label("Drag to position photo")
                        .aria_description("Arrow keys position the photo; Tab moves to zoom")
                        .child({
                            let (preview, center) = (preview.clone(), self.center);
                            canvas(
                                |_, _, _| {},
                                move |bounds, _, window, _| {
                                    let image_bounds = Bounds::new(
                                        point(
                                            bounds.left() + px(60. - width * center[0]),
                                            bounds.top() + px(60. - height * center[1]),
                                        ),
                                        size(px(width), px(height)),
                                    );
                                    // Clip the image itself; a rounded parent only clips to its rectangle.
                                    let _ = window.paint_image(
                                        bounds,
                                        image_bounds,
                                        Corners::all(px(60.)),
                                        preview.clone(),
                                        0,
                                        false,
                                    );
                                },
                            )
                            .size_full()
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, event: &MouseDownEvent, w, cx| {
                                this.focus.focus(w, cx);
                                this.dragging = Some(event.position);
                                cx.stop_propagation();
                            }),
                        )
                        .test_support(),
                )
                .child(
                    Slider::new(&self.zoom)
                        .accessibility_label("Photo zoom")
                        .w(px(180.)),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("agent-icon-cancel")
                                .ghost()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cancel(cx);
                                    cx.emit(Event::Dismiss);
                                })),
                        )
                        .child(
                            Button::new("agent-icon-done")
                                .primary()
                                .label(if self.busy { "Saving…" } else { "Done" })
                                .on_click(cx.listener(|this, _, w, cx| this.done(w, cx))),
                        ),
                )
                .into_any_element()
        } else {
            v_flex()
                .items_center()
                .justify_center()
                .gap_3()
                .h(px(210.))
                .w_full()
                .border_1()
                .border_color(theme.border)
                .rounded(px(8.))
                .child(if self.busy {
                    "Loading image…"
                } else {
                    "Drop, paste or choose an image"
                })
                .child(
                    Button::new("agent-icon-choose")
                        .label("Choose…")
                        .on_click(cx.listener(|this, _, w, cx| this.choose(w, cx))),
                )
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .text_xs()
                        .child("⌘V · PNG, JPEG, WebP, GIF, HEIC · 10 MB"),
                )
                .into_any_element()
        };
        v_flex()
            .gap_2()
            .pt_3()
            .child(content)
            .children(self.message.as_ref().map(|m| {
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(m.clone())
            }))
            .into_any_element()
    }
}

impl Render for IconPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let border = if self.dropping.is_some() {
            theme.primary
        } else {
            theme.border
        };
        let muted = theme.muted_foreground;
        let mut body = v_flex()
            .id("agent-icon-picker")
            .track_focus(&self.focus)
            .role(Role::Dialog)
            .aria_label("Change agent icon")
            .w(px(320.))
            .h(px(360.))
            .p(px(15.))
            .gap_2()
            .rounded(px(12.))
            .border_1()
            .border_color(border)
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .shadow_lg()
            .child(
                h_flex()
                    .gap_1()
                    .children(
                        [
                            (Tab::Icons, "Icons"),
                            (Tab::Emoji, "Emoji"),
                            (Tab::Image, "Image"),
                        ]
                        .into_iter()
                        .map(|(tab, label)| {
                            Button::new(label)
                                .small()
                                .ghost()
                                .label(label)
                                .when(self.tab == tab, |b| b.bg(theme.accent))
                                .on_click(
                                    cx.listener(move |this, _, w, cx| this.change_tab(tab, w, cx)),
                                )
                        }),
                    )
                    .child(div().flex_1())
                    .when(self.current != AgentIcon::default(), |d| {
                        d.child(
                            Button::new("agent-icon-remove")
                                .ghost()
                                .small()
                                .label("Remove")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cancel(cx);
                                    cx.emit(Event::Apply(AgentIcon::default()));
                                })),
                        )
                    }),
            );
        if self.tab == Tab::Image {
            body = body.child(self.image_tab(cx));
        } else {
            body = body.child(
                Input::new(&self.search).prefix(Icon::new(IconName::Search).with_size(px(14.))),
            );
            if self.tab == Tab::Icons {
                body = body.child(h_flex().gap_2().children(TINTS.iter().map(|token| {
                    let token = *token;
                    let selected = self.tint.as_deref().unwrap_or("neutral") == token;
                    Button::new(format!("agent-icon-tint-{token}"))
                        .ghost()
                        .accessibility_label(format!("{token} tint"))
                        .w(px(22.))
                        .h(px(22.))
                        .p(px(3.))
                        .rounded_full()
                        .border_1()
                        .border_color(if selected {
                            theme.foreground
                        } else {
                            theme.transparent
                        })
                        .cursor_pointer()
                        .child(
                            div()
                                .size_full()
                                .rounded_full()
                                .bg(tint(token, theme).unwrap()),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.tint = (token != "neutral").then(|| token.into());
                            cx.notify();
                        }))
                })));
            } else {
                body = body.child(
                    h_flex()
                        .gap_1()
                        .child(div().text_xs().text_color(muted).child("Skin tone"))
                        .children(
                            ["🖐️", "🖐🏻", "🖐🏼", "🖐🏽", "🖐🏾", "🖐🏿"]
                                .into_iter()
                                .enumerate()
                                .map(|(tone, label)| {
                                    Button::new(("agent-icon-tone", tone))
                                        .ghost()
                                        .xsmall()
                                        .label(label)
                                        .when(self.preferences.tone == tone, |b| b.bg(theme.accent))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.preferences.tone = tone;
                                            this.preferences.save();
                                            this.filter(cx);
                                            cx.notify();
                                        }))
                                }),
                        ),
                );
                if self.search.read(cx).value().is_empty() && !self.preferences.frequent.is_empty()
                {
                    body =
                        body.child(div().text_xs().text_color(muted).child("Frequently used"))
                            .child(
                                h_flex().children(
                                    self.preferences
                                        .frequent
                                        .iter()
                                        .take(COLUMNS)
                                        .filter_map(|(value, _)| emojis::get(value))
                                        .map(|emoji| {
                                            Button::new(SharedString::from(format!(
                                                "frequent-{}",
                                                emoji.as_str()
                                            )))
                                            .ghost()
                                            .xsmall()
                                            .label(emoji.as_str())
                                            .tooltip(emoji.name())
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.pick_emoji(emoji, cx)
                                            }))
                                        }),
                                ),
                            );
                }
            }
            body = body.child(
                uniform_list(
                    "agent-icon-grid",
                    self.choices.len().div_ceil(COLUMNS),
                    cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|row| {
                                h_flex().h(px(32.)).children(
                                    (row * COLUMNS..(row + 1) * COLUMNS)
                                        .map(|index| this.cell(index, cx)),
                                )
                            })
                            .collect()
                    }),
                )
                .track_scroll(&self.scroll)
                .flex_1()
                .min_h_0(),
            );
            if self.choices.is_empty() {
                body = body.child(div().text_color(muted).child("No matches"));
            }
        }
        let weak = cx.entity().downgrade();
        body.on_drag_move(
            cx.listener(|this, event: &DragMoveEvent<ExternalPaths>, _, cx| {
                this.dropping = if event.bounds.contains(&event.event.position) {
                    event.drag(cx).paths().first().cloned()
                } else {
                    None
                };
                cx.notify();
            }),
        )
        .on_file_drop_exit(cx.listener(|this, _, _, cx| {
            this.dropping = None;
            cx.notify();
        }))
        .child(
            canvas(
                |_, _, _| {},
                move |_, _, window, _| {
                    let moving = weak.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                        if phase != DispatchPhase::Bubble {
                            return;
                        }
                        moving
                            .update(cx, |this, cx| {
                                if event.pressed_button != Some(MouseButton::Left) {
                                    this.dragging = None;
                                    return;
                                }
                                if let (Some(previous), Some(image)) = (this.dragging, &this.image)
                                {
                                    let side = image.width().min(image.height()) as f32
                                        / this.zoom.read(cx).value().start();
                                    this.center[0] -=
                                        f32::from(event.position.x - previous.x) / 120. * side
                                            / image.width() as f32;
                                    this.center[1] -=
                                        f32::from(event.position.y - previous.y) / 120. * side
                                            / image.height() as f32;
                                    this.dragging = Some(event.position);
                                    this.clamp_center(cx);
                                    cx.notify();
                                }
                            })
                            .ok();
                    });
                    let weak = weak.clone();
                    window.on_mouse_event(move |_: &MouseUpEvent, phase, w, cx| {
                        if phase != DispatchPhase::Bubble {
                            return;
                        }
                        weak.update(cx, |this, cx| {
                            this.dragging = None;
                            if let Some(path) = this.dropping.take() {
                                this.load_path(&path, true, w, cx);
                            }
                        })
                        .ok();
                    });
                },
            )
            .absolute()
            .size_0(),
        )
        .test_support()
        .focus_trap("agent-icon-picker", &self.focus)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[core::prelude::v1::test]
    fn curated_symbols_and_search_are_complete() {
        assert!((150..=250).contains(&SYMBOLS.len()));
        let mut names = HashSet::new();
        for (name, icon) in SYMBOLS {
            assert!(names.insert(name));
            assert_eq!(symbol(name), *icon);
        }
        assert!(symbol_matches("bot", "robot"));
        assert!(symbol_matches("terminal", "developer"));
        assert!(!symbol_matches("terminal", "star"));
        assert_eq!(symbol("not-an-icon"), IconName::Bot);
        assert!(emojis::iter().any(|emoji| emoji.name() == "penguin"));
    }
}

#[cfg(all(test, feature = "test-support"))]
mod regression_tests {
    use super::*;
    use core::prelude::v1::test;

    fn picker(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<IconPicker>) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
                cx.new(|cx| {
                    IconPicker::new(AgentIcon::default(), "picker-regression".into(), w, cx)
                })
            })
            .unwrap()
        })
    }

    #[gpui_kit::test]
    fn emoji_search_matches_mixed_case_unicode_names(cx: &mut TestAppContext) {
        let _turn = super::super::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        super::super::ui_tests::test_home();
        let (window, picker) = picker(cx);
        cx.update_window(window, |_, w, cx| {
            picker.update(cx, |this, cx| {
                this.change_tab(Tab::Emoji, w, cx);
                this.search
                    .update(cx, |input, cx| input.set_value("JAPANESE POST", w, cx));
                this.filter(cx);
                assert!(
                    this.choices.iter().any(
                        |choice| matches!(choice, Choice::Emoji(emoji) if emoji.as_str() == "🏣")
                    ),
                    "Search should match Japanese post office regardless of case"
                );
            });
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn enter_on_native_controls_does_not_select_grid(cx: &mut TestAppContext) {
        let _turn = super::super::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        super::super::ui_tests::test_home();
        let (window, picker) = picker(cx);
        cx.update_window(window, |_, w, cx| {
            let button_focus = cx.focus_handle();
            button_focus.focus(w, cx);
            picker.update(cx, |this, cx| {
                assert!(
                    !this.key(&Keystroke::parse("enter").unwrap(), w, cx),
                    "Enter belongs to the focused button, not the grid"
                );
            });
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn empty_paste_clears_previous_crop(cx: &mut TestAppContext) {
        let _turn = super::super::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        super::super::ui_tests::test_home();
        let (window, picker) = picker(cx);
        cx.update_window(window, |_, w, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(String::new()));
            picker.update(cx, |this, cx| {
                this.image = Some(Arc::new(image::RgbaImage::new(10, 20)));
                this.paste(w, cx);
                assert!(
                    this.image.is_none(),
                    "Empty paste must not leave the previous image ready to apply"
                );
                assert_eq!(this.message.as_deref(), Some("No image on the pasteboard"));
            });
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn changing_tabs_invalidates_pending_image_save(cx: &mut TestAppContext) {
        let _turn = super::super::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        super::super::ui_tests::test_home();
        let (window, picker) = picker(cx);
        let applies = Rc::new(std::cell::Cell::new(0));
        let observed = applies.clone();
        let _events = cx.update(|cx| {
            cx.subscribe(&picker, move |_, event, _| {
                if matches!(event, Event::Apply(_)) {
                    observed.set(observed.get() + 1);
                }
            })
        });
        cx.update_window(window, |_, w, cx| {
            picker.update(cx, |this, cx| {
                this.image = Some(Arc::new(image::RgbaImage::new(10, 20)));
                this.done(w, cx);
                this.change_tab(Tab::Emoji, w, cx);
            });
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(
            applies.get(),
            0,
            "A superseded save must not replace the new choice"
        );
    }
}
