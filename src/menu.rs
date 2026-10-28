//! Right-click menus drawn after Linear's: 32px rows in an inset rounded highlight,
//! muted 16px icons, 1px separators, metadata right-aligned in the muted colour.
//! gpui-kit's `PopupMenu` hard-codes its row height, 2px separators and icon colour.
#[cfg(test)]
use core::prelude::v1::test;
use gpui_kit::assets::IconName;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon, Sizable as _, Theme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

type OnClick = Rc<dyn Fn(&mut Window, &mut App)>;

pub(crate) enum Entry {
    Item {
        icon: Option<IconName>,
        /// Drawn instead of `icon`: an app's own icon.
        image: Option<Arc<Image>>,
        label: SharedString,
        /// Right-aligned and muted: the branch a copy will copy, the path an open will open.
        hint: Option<SharedString>,
        /// A setting that's on, or the current choice: a check at the right end.
        checked: bool,
        on_click: OnClick,
    },
    Sub {
        icon: IconName,
        image: Option<Arc<Image>>,
        label: SharedString,
        hint: Option<SharedString>,
        entries: Vec<Entry>,
    },
    Sep,
}

pub(crate) fn item(
    icon: impl Into<Option<IconName>>,
    label: impl Into<SharedString>,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> Entry {
    Entry::Item {
        icon: icon.into(),
        image: None,
        label: label.into(),
        hint: None,
        checked: false,
        on_click: Rc::new(on_click),
    }
}

pub(crate) fn sub(icon: IconName, label: impl Into<SharedString>, entries: Vec<Entry>) -> Entry {
    Entry::Sub {
        icon,
        image: None,
        label: label.into(),
        hint: None,
        entries,
    }
}

impl Entry {
    /// An empty `text` shows no hint.
    pub(crate) fn hint(mut self, text: impl Into<SharedString>) -> Self {
        let text = text.into();
        if let Entry::Item { hint, .. } | Entry::Sub { hint, .. } = &mut self {
            *hint = (!text.is_empty()).then_some(text);
        }
        self
    }

    /// `None` keeps the icon.
    pub(crate) fn image(mut self, img: Option<Arc<Image>>) -> Self {
        if let Entry::Item { image, .. } | Entry::Sub { image, .. } = &mut self {
            *image = img;
        }
        self
    }

    pub(crate) fn checked(mut self, on: bool) -> Self {
        if let Entry::Item { checked, .. } = &mut self {
            *checked = on;
        }
        self
    }
}

/// Opens `entries` at `pos` and focuses them; draw the menu with [`layer`] until
/// it emits `DismissEvent`. Closing hands focus back to whatever had it, before
/// the picked item runs so the item can move focus itself (a dialog, the composer).
pub(crate) fn open(
    entries: Vec<Entry>,
    pos: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<Menu> {
    open_at(entries, pos, Anchor::TopLeft, window, cx)
}

/// [`open`] with the menu's `corner` at `pos`: a dropdown at the window's right edge hangs
/// from its button's right end (`Anchor::TopRight`).
pub(crate) fn open_at(
    entries: Vec<Entry>,
    pos: Point<Pixels>,
    corner: Anchor,
    window: &mut Window,
    cx: &mut App,
) -> Entity<Menu> {
    let prev = window.focused(cx);
    let focus = cx.focus_handle();
    // Deferred: the right-click is still bubbling, and the sidebar's `track_focus`
    // would take focus back on its way up.
    window.defer(cx, {
        let focus = focus.clone();
        move |window, cx| focus.focus(window, cx)
    });
    cx.new(|_| Menu {
        entries,
        pos,
        corner,
        focus,
        prev,
        sel: None,
        typed: String::new(),
        sub: None,
        sub_sel: None,
        last_x: px(0.),
        replacement: false,
        scroll: ScrollHandle::new(),
        sub_scroll: ScrollHandle::new(),
        bounds: Rc::new(Cell::new(Bounds::default())),
        closed: false,
        viewport: window.viewport_size(),
    })
}

/// Pinned to the window's corner, so the click catcher covers the window wherever it's drawn.
pub(crate) fn layer(menu: Entity<Menu>) -> impl IntoElement {
    deferred(anchored().position(point(px(0.), px(0.))).child(menu)).with_priority(POPUP_PRIORITY)
}

const POPUP_PRIORITY: usize = 100;

pub(crate) struct Menu {
    entries: Vec<Entry>,
    pos: Point<Pixels>,
    corner: Anchor,
    focus: FocusHandle,
    prev: Option<FocusHandle>,
    /// Highlighted row; the mouse and the arrow keys share it, as in Linear.
    sel: Option<usize>,
    /// Letters typed so far: like a Mac menu, typing jumps to the row that starts with them.
    typed: String,
    /// Open submenu, and its highlighted row once the keyboard has moved into it.
    sub: Option<usize>,
    sub_sel: Option<usize>,
    last_x: Pixels,
    replacement: bool,
    scroll: ScrollHandle,
    sub_scroll: ScrollHandle,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    closed: bool,
    viewport: Size<Pixels>,
}

impl EventEmitter<DismissEvent> for Menu {}

/// Next row up or down from `from`, wrapping and skipping separators.
fn step(entries: &[Entry], from: Option<usize>, down: bool) -> Option<usize> {
    let n = entries.len();
    let mut ix = from.unwrap_or(if down { n.wrapping_sub(1) } else { 0 });
    for _ in 0..n {
        ix = if down { (ix + 1) % n } else { (ix + n - 1) % n };
        if !matches!(entries[ix], Entry::Sep) {
            return Some(ix);
        }
    }
    None
}

/// The first row from `from` on, wrapping, whose label starts with `typed` (lowercase).
fn find(entries: &[Entry], typed: &str, from: usize) -> Option<usize> {
    let n = entries.len();
    (0..n)
        .map(|i| (from + i) % n)
        .find(|&ix| match &entries[ix] {
            Entry::Item { label, .. } | Entry::Sub { label, .. } => {
                label.to_lowercase().starts_with(typed)
            }
            Entry::Sep => false,
        })
}

impl Menu {
    /// Starts the highlight on `ix`, e.g. the current choice of a picker.
    pub(crate) fn select(&mut self, ix: usize) {
        self.sel = Some(ix);
        self.scroll.scroll_to_item(ix);
    }

    /// The row whose label starts with what was typed, ignoring case. A new letter looks
    /// past the highlight, so typing it again cycles through the rows it starts ("h": Haiku,
    /// High); more letters narrow from the highlight on ("hi": High).
    fn jump(&mut self, ch: &str) -> bool {
        let ch = ch.to_lowercase();
        let more = !self.typed.is_empty();
        self.typed.push_str(&ch);
        let in_sub = self.replacement || self.sub_sel.is_some();
        let sel = if in_sub { self.sub_sel } else { self.sel };
        let entries = if in_sub {
            self.sub_entries()
        } else {
            &self.entries
        };
        let hit = more
            .then(|| find(entries, &self.typed, sel.unwrap_or(0)))
            .flatten()
            .or_else(|| find(entries, &ch, sel.map_or(0, |s| s + 1)));
        if !more || find(entries, &self.typed, sel.unwrap_or(0)).is_none() {
            self.typed = ch;
        }
        if let Some(ix) = hit {
            if in_sub {
                self.sub_sel = Some(ix);
                self.sub_scroll.scroll_to_item(ix);
            } else {
                self.sel = Some(ix);
                self.scroll.scroll_to_item(ix);
                self.sub = None;
                self.sub_sel = None;
                self.replacement = false;
            }
        }
        hit.is_some()
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.closed = true;
        if let Some(f) = self.prev.take() {
            f.focus(window, cx);
        }
        cx.emit(DismissEvent);
    }

    fn sub_entries(&self) -> &[Entry] {
        match self.sub.and_then(|ix| self.entries.get(ix)) {
            Some(Entry::Sub { entries, .. }) => entries,
            _ => &[],
        }
    }

    fn placement(&self, ix: usize, window: &Window, cx: &App) -> Option<ChildPlacement> {
        let Entry::Sub { entries, .. } = &self.entries[ix] else {
            return None;
        };
        child_placement(
            self.bounds.get(),
            panel_width(entries, window, cx),
            window.viewport_size().width,
        )
    }

    fn back(&mut self) {
        self.sub = None;
        self.sub_sel = None;
        self.replacement = false;
        self.typed.clear();
    }

    /// Runs an item, or opens a submenu with its first row highlighted.
    fn activate(&mut self, ix: usize, in_sub: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        let list = if in_sub {
            self.sub_entries()
        } else {
            &self.entries
        };
        match list.get(ix) {
            Some(Entry::Item { on_click, .. }) => {
                let f = on_click.clone();
                self.close(window, cx);
                f(window, cx);
            }
            Some(Entry::Sub { entries, .. }) if !in_sub => {
                self.sub_sel = step(entries, None, true);
                self.replacement = self.placement(ix, window, cx).is_none();
                self.sel = Some(ix);
                self.sub = Some(ix);
                self.typed.clear();
                self.sub_scroll.set_offset(Point::default());
                if let Some(sel) = self.sub_sel {
                    self.sub_scroll.scroll_to_item(sel);
                }
                cx.notify();
            }
            _ => {}
        }
    }

    fn hover(&mut self, ix: usize, x: Pixels, window: &Window, cx: &mut Context<Self>) {
        // shortcut: horizontal direction protects diagonal traversal; use a triangle if pointer testing needs it.
        let toward_sub = self
            .sub
            .filter(|&s| s != ix)
            .and_then(|s| self.placement(s, window, cx))
            .is_some_and(|p| {
                if p.left {
                    x < self.last_x
                } else {
                    x > self.last_x
                }
            });
        self.last_x = x;
        if toward_sub {
            return;
        }
        let sub = (matches!(self.entries[ix], Entry::Sub { .. })
            && self.placement(ix, window, cx).is_some())
        .then_some(ix);
        if self.sel == Some(ix) && self.sub == sub {
            return;
        }
        self.sel = Some(ix);
        self.sub = sub;
        self.sub_sel = None;
        self.typed.clear();
        self.sub_scroll.set_offset(Point::default());
        cx.notify();
    }

    fn key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let in_sub = self.replacement || self.sub_sel.is_some();
        match e.keystroke.key.as_str() {
            "escape" | "left" if self.sub.is_some() => self.back(),
            "escape" => self.close(window, cx),
            "up" | "down" if in_sub => {
                self.sub_sel = step(self.sub_entries(), self.sub_sel, e.keystroke.key == "down");
                if let Some(ix) = self.sub_sel {
                    self.sub_scroll.scroll_to_item(ix);
                }
                self.typed.clear();
            }
            "up" | "down" => {
                self.sel = step(&self.entries, self.sel, e.keystroke.key == "down");
                self.back();
                if let Some(ix) = self.sel {
                    self.scroll.scroll_to_item(ix);
                }
            }
            "right" if !in_sub => match self.sel {
                Some(ix) if matches!(self.entries[ix], Entry::Sub { .. }) => {
                    self.activate(ix, false, window, cx)
                }
                _ => return,
            },
            "enter" | "space" => match (in_sub, self.sub_sel, self.sel) {
                (true, Some(ix), _) | (false, _, Some(ix)) => self.activate(ix, in_sub, window, cx),
                _ => return,
            },
            k if k.chars().count() == 1 && !e.keystroke.modifiers.modified() && self.jump(k) => {}
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn panel(
        &self,
        entries: &[Entry],
        sel: Option<usize>,
        in_sub: bool,
        width: Pixels,
        window: &Window,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let (icons, controls) = columns(entries);
        let rows = entries.iter().enumerate().map(|(ix, e)| {
            let (icon, image, label, hint, checked, submenu) = match e {
                Entry::Sep => {
                    return div()
                        .h(px(1.))
                        .my(px(6.))
                        .mx(px(-6.))
                        .bg(theme.foreground.alpha(0.08))
                        .into_any_element();
                }
                Entry::Item {
                    icon,
                    image,
                    label,
                    hint,
                    checked,
                    ..
                } => (*icon, image.clone(), label, hint, *checked, false),
                Entry::Sub {
                    icon,
                    image,
                    label,
                    hint,
                    ..
                } => (Some(*icon), image.clone(), label, hint, false, true),
            };
            let label_width = text_width(label, window, theme);
            let hint_width = hint
                .as_ref()
                .map_or(px(0.), |h| text_width(h, window, theme));
            let (label_space, hint_space) = text_columns(
                width,
                icons,
                controls,
                label_width,
                hint_width,
                hint.is_some(),
            );
            let open = !in_sub && self.sub == Some(ix);
            let selected = sel == Some(ix) || open;
            let background = if selected {
                theme.popover.blend(highlight(theme))
            } else {
                theme.popover
            };
            let clipped = label_width > label_space || hint_width > hint_space;
            let tip: SharedString = match hint {
                Some(h) => format!("{label} · {h}").into(),
                None => label.clone(),
            };
            h_flex()
                .id(ix)
                .role(Role::MenuItem)
                .aria_label(label.clone())
                .when_some(hint.clone(), |d, hint| d.aria_description(hint))
                .relative()
                .h(ROW_H)
                .flex_none()
                .px(px(10.))
                .gap(px(8.))
                .rounded(theme.radius)
                .when(selected, |d| d.bg(highlight(theme)))
                .when(icons, |d| {
                    d.child(
                        div()
                            .id(("menu-icon", ix))
                            .test_support()
                            .size(px(16.))
                            .flex_none()
                            .child(match (image, icon) {
                                // App icons have a margin built in; 18px matches a 16px glyph.
                                (Some(app), _) => img(app)
                                    .size(px(18.))
                                    .mx(px(-1.))
                                    .flex_none()
                                    .into_any_element(),
                                (None, Some(i)) => Icon::new(i)
                                    .with_size(px(16.))
                                    .text_color(crate::ui::faint(theme))
                                    .into_any_element(),
                                (None, None) => div().size(px(16.)).into_any_element(),
                            }),
                    )
                })
                .child(
                    crate::overflow_fade::label(label.clone(), background, None)
                        .id(("menu-label", ix))
                        .test_support()
                        .w(label_space)
                        .flex_none(),
                )
                .children(hint.as_ref().map(|h| {
                    crate::overflow_fade::label(h.clone(), background, None)
                        .id(("menu-hint", ix))
                        .test_support()
                        .w(hint_space)
                        .flex_none()
                        .text_color(theme.muted_foreground)
                }))
                .when(controls, |d| {
                    d.child(
                        div()
                            .id(("menu-control", ix))
                            .test_support()
                            .size(px(16.))
                            .flex_none()
                            .when(checked || submenu, |d| {
                                d.child(
                                    Icon::new(if submenu {
                                        IconName::ChevronRight
                                    } else {
                                        IconName::Check
                                    })
                                    .with_size(px(16.))
                                    .text_color(theme.muted_foreground),
                                )
                            }),
                    )
                })
                .when(clipped, |d| {
                    d.tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
                })
                .on_mouse_move(cx.listener(move |this, e: &MouseMoveEvent, w, cx| {
                    if in_sub {
                        if this.sub_sel != Some(ix) {
                            this.sub_sel = Some(ix);
                            cx.notify();
                        }
                    } else {
                        this.hover(ix, e.position.x, w, cx);
                    }
                }))
                .on_click(cx.listener(move |this, _, w, cx| this.activate(ix, in_sub, w, cx)))
                .test_support()
                .into_any_element()
        });
        let back = in_sub && self.replacement;
        let bounds = self.bounds.clone();
        chrome(theme)
            .id(if in_sub { "menu-child" } else { "menu-parent" })
            .role(Role::Menu)
            .occlude()
            .relative()
            .rounded(theme.radius_lg)
            .w(width)
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .when(back, |d| {
                d.child(
                    h_flex()
                        .id("menu-back")
                        .aria_label("Back")
                        .test_support()
                        .role(Role::MenuItem)
                        .h(ROW_H)
                        .flex_none()
                        .px(px(10.))
                        .gap(px(8.))
                        .rounded(theme.radius)
                        .hover(|s| s.bg(highlight(theme)))
                        .child(Icon::new(IconName::ChevronLeft).with_size(px(16.)))
                        .child("Back")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.back();
                            cx.notify();
                        })),
                )
            })
            .child(
                div()
                    .id(if in_sub {
                        "menu-child-scroll"
                    } else {
                        "menu-parent-scroll"
                    })
                    .max_h(
                        (window.viewport_size().height
                            - px(28.)
                            - if back { ROW_H } else { px(0.) })
                        .max(px(0.)),
                    )
                    .overflow_y_scroll()
                    .overflow_x_hidden()
                    .track_scroll(if in_sub {
                        &self.sub_scroll
                    } else {
                        &self.scroll
                    })
                    .children(rows.collect::<Vec<_>>())
                    .on_scroll_wheel(cx.listener(move |this, _, _, cx| {
                        if !in_sub && this.sub.is_some() {
                            this.back();
                            cx.notify();
                        }
                        cx.stop_propagation();
                    }))
                    .test_support(),
            )
            .when(!in_sub, |d| {
                d.child(
                    canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {})
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                )
            })
            .test_support()
    }
}

fn columns(entries: &[Entry]) -> (bool, bool) {
    let icons = entries.iter().any(|e| {
        matches!(
            e,
            Entry::Item { icon: Some(_), .. }
                | Entry::Item { image: Some(_), .. }
                | Entry::Sub { .. }
        )
    });
    let controls = entries
        .iter()
        .any(|e| matches!(e, Entry::Item { checked: true, .. } | Entry::Sub { .. }));
    (icons, controls)
}

fn text_width(text: &SharedString, window: &Window, theme: &Theme) -> Pixels {
    let mut style = window.text_style();
    style.font_family = theme.font_family.clone();
    window
        .text_system()
        .shape_line(text.clone(), px(14.), &[style.to_run(text.len())], None)
        .width
        .ceil()
}

/// Includes panel/row padding and fixed icon/control columns with their gaps.
fn fixed_width(icons: bool, controls: bool) -> Pixels {
    px(32. + if icons { 24. } else { 0. } + if controls { 24. } else { 0. })
}

fn measured_panel_width(
    preferred: Pixels,
    label: Pixels,
    fixed: Pixels,
    viewport: Pixels,
) -> Pixels {
    preferred
        .max(px(200.))
        .min(px(440.))
        .max(label + fixed)
        .min((viewport - px(16.)).max(px(0.)))
}

fn panel_width(entries: &[Entry], window: &Window, cx: &App) -> Pixels {
    let (icons, controls) = columns(entries);
    let fixed = fixed_width(icons, controls);
    let mut preferred = px(0.);
    let mut longest_label = px(0.);
    for entry in entries {
        if let Entry::Item { label, hint, .. } | Entry::Sub { label, hint, .. } = entry {
            let label = text_width(label, window, cx.theme());
            // Keep the column gap even when metadata has yielded all of its width.
            let gap = if hint.is_some() { px(8.) } else { px(0.) };
            longest_label = longest_label.max(label + gap);
            let hint = hint
                .as_ref()
                .map_or(px(0.), |h| text_width(h, window, cx.theme()).min(px(180.)));
            preferred = preferred.max(fixed + label + gap + hint);
        }
    }
    measured_panel_width(preferred, longest_label, fixed, window.bounds().size.width)
}

fn text_columns(
    width: Pixels,
    icons: bool,
    controls: bool,
    label: Pixels,
    hint: Pixels,
    has_hint: bool,
) -> (Pixels, Pixels) {
    let available =
        (width - fixed_width(icons, controls) - if has_hint { px(8.) } else { px(0.) }).max(px(0.));
    let hint = hint.min(px(180.)).min((available - label).max(px(0.)));
    (available - hint, hint)
}

/// The look every floating menu shares (this one and the composer's completions): `popover`,
/// 6px inset, 14px text, 8px corners, and a translucent ring instead of a border, so the shadow shows
/// through the edge.
pub(crate) fn chrome(theme: &Theme) -> Div {
    let ink = hsla(0., 0., 0., if theme.is_dark() { 0.5 } else { 0.12 });
    v_flex()
        .p(px(6.))
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .text_size(px(14.))
        .rounded(theme.radius)
        .shadow(vec![
            BoxShadow::new(px(0.), px(0.), theme.foreground.alpha(0.1)).spread_radius(px(1.)),
            BoxShadow::new(px(0.), px(4.), ink).blur_radius(px(8.)),
            BoxShadow::new(px(0.), px(1.), ink.alpha(ink.a / 2.)).blur_radius(px(1.)),
        ])
}

/// A menu row's height; the composer's completions use it too.
pub(crate) const ROW_H: Pixels = px(32.);

/// The highlighted row uses a translucent fill, leaving the text colour unchanged.
pub(crate) fn highlight(theme: &Theme) -> Hsla {
    theme.foreground.alpha(0.07)
}

#[derive(Clone, Copy, Debug)]
struct ChildPlacement {
    left: bool,
    x: Pixels,
    width: Pixels,
}

fn child_placement(
    parent: Bounds<Pixels>,
    desired: Pixels,
    viewport: Pixels,
) -> Option<ChildPlacement> {
    let right = (viewport - px(8.) - parent.right() - px(4.)).max(px(0.));
    let left = (parent.left() - px(12.)).max(px(0.));
    if right.max(left) < px(200.) {
        return None;
    }
    let leftward = right < desired && (left >= desired || left > right);
    let width = desired.min(if leftward { left } else { right });
    Some(ChildPlacement {
        left: leftward,
        x: if leftward {
            parent.left() - px(4.) - width
        } else {
            parent.right() + px(4.)
        },
        width,
    })
}

fn panel_height(entries: &[Entry], back: bool, viewport: Pixels) -> Pixels {
    let rows = entries
        .iter()
        .map(|e| {
            if matches!(e, Entry::Sep) {
                px(13.)
            } else {
                ROW_H
            }
        })
        .sum::<Pixels>();
    (rows + px(12.) + if back { ROW_H } else { px(0.) }).min((viewport - px(16.)).max(px(0.)))
}

fn panel_position(
    pos: Point<Pixels>,
    corner: Anchor,
    size: Size<Pixels>,
    viewport: Size<Pixels>,
) -> Point<Pixels> {
    let x = pos.x
        - if matches!(corner, Anchor::TopRight | Anchor::BottomRight) {
            size.width
        } else {
            px(0.)
        };
    let y = pos.y
        - if matches!(corner, Anchor::BottomLeft | Anchor::BottomRight) {
            size.height
        } else {
            px(0.)
        };
    point(
        x.max(px(8.))
            .min((viewport.width - size.width - px(8.)).max(px(8.))),
        y.max(px(8.))
            .min((viewport.height - size.height - px(8.)).max(px(8.))),
    )
}

impl Render for Menu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.viewport != window.viewport_size() {
            if !self.replacement {
                self.back();
            }
            self.viewport = window.viewport_size();
        }
        let child = self.sub.and_then(|ix| {
            if self.replacement {
                return None;
            }
            let placement = self.placement(ix, window, cx)?;
            let row = self.scroll.bounds_for_item(ix)?;
            let y = row.top() + self.scroll.offset().y - px(6.);
            let position = panel_position(
                point(placement.x, y),
                Anchor::TopLeft,
                size(
                    placement.width,
                    panel_height(self.sub_entries(), false, window.viewport_size().height),
                ),
                window.viewport_size(),
            );
            Some(
                deferred(anchored().position(position).child(self.panel(
                    self.sub_entries(),
                    self.sub_sel,
                    true,
                    placement.width,
                    window,
                    cx,
                )))
                .with_priority(POPUP_PRIORITY + 1),
            )
        });
        let (entries, sel) = if self.replacement {
            (self.sub_entries(), self.sub_sel)
        } else {
            (&self.entries[..], self.sel)
        };
        let width = panel_width(entries, window, cx);
        let position = panel_position(
            self.pos,
            self.corner,
            size(
                width,
                panel_height(entries, self.replacement, window.viewport_size().height),
            ),
            window.viewport_size(),
        );
        // A window-sized catcher consumes outside clicks before restoring focus.
        div()
            .occlude()
            .w(window.viewport_size().width)
            .h(window.viewport_size().height)
            .track_focus(&self.focus)
            .key_context("SharkMenu")
            .on_key_down(cx.listener(Self::key))
            .on_any_mouse_down(cx.listener(|this, _, w, cx| {
                cx.stop_propagation();
                this.close(w, cx);
            }))
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .child(anchored().position(position).child(self.panel(
                entries,
                sel,
                self.replacement,
                width,
                window,
                cx,
            )))
            .children(child)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submenu_placement_uses_real_room_and_replaces_when_neither_side_is_usable() {
        let parent = Bounds::new(point(px(350.), px(8.)), size(px(300.), px(100.)));
        let p = child_placement(parent, px(200.), px(1000.)).unwrap();
        assert!(!p.left);
        assert_eq!(p.x, px(654.));
        let p = child_placement(parent, px(400.), px(900.)).unwrap();
        assert!(p.left);
        assert_eq!(p.width, px(338.));
        assert_eq!(p.x, px(8.));
        let parent = Bounds::new(point(px(100.), px(8.)), size(px(300.), px(100.)));
        let p = child_placement(parent, px(440.), px(700.)).unwrap();
        assert!(!p.left);
        assert_eq!(p.width, px(288.));
        assert!(child_placement(parent, px(200.), px(600.)).is_none());
    }

    #[test]
    fn panel_margin_is_enforced_even_without_window_overflow() {
        let viewport = size(px(360.), px(240.));
        let panel = size(px(344.), px(224.));
        for corner in [
            Anchor::TopLeft,
            Anchor::TopRight,
            Anchor::BottomLeft,
            Anchor::BottomRight,
        ] {
            for pos in [
                point(px(0.), px(0.)),
                point(px(16.), px(16.)),
                point(px(360.), px(240.)),
            ] {
                assert_eq!(
                    panel_position(pos, corner, panel, viewport),
                    point(px(8.), px(8.))
                );
            }
        }
    }

    #[test]
    fn panel_width_preserves_labels_until_the_window_constrains_them() {
        let fixed = fixed_width(true, true);
        assert_eq!(
            measured_panel_width(px(150.), px(80.), fixed, px(1000.)),
            px(200.)
        );
        assert_eq!(
            measured_panel_width(px(550.), px(250.), fixed, px(1000.)),
            px(440.)
        );
        assert_eq!(
            measured_panel_width(px(800.), px(500.), fixed, px(1000.)),
            px(580.)
        );
        assert_eq!(
            measured_panel_width(px(800.), px(500.), fixed, px(300.)),
            px(284.)
        );
    }

    #[test]
    fn text_columns_yield_metadata_before_labels_and_keep_controls_disjoint() {
        let fixed = fixed_width(true, true);
        for width in [px(200.), px(340.), px(440.)] {
            let (label, hint) = text_columns(width, true, true, px(250.), px(180.), true);
            assert_eq!(label + hint + px(8.) + fixed, width);
            assert!(label >= px(250.).min(width - fixed - px(8.)));
            assert!(hint >= px(0.));
        }
        assert_eq!(
            text_columns(px(340.), true, true, px(250.), px(180.), true),
            (px(250.), px(2.))
        );
        assert_eq!(
            text_columns(px(200.), true, true, px(250.), px(180.), true),
            (px(112.), px(0.))
        );
    }

    #[test]
    fn step_skips_separators_and_wraps() {
        let e = || item(None, "x", |_, _| {});
        let entries = [e(), Entry::Sep, e(), Entry::Sep];
        assert_eq!(step(&entries, None, true), Some(0));
        assert_eq!(step(&entries, Some(0), true), Some(2));
        assert_eq!(step(&entries, Some(2), true), Some(0));
        assert_eq!(step(&entries, None, false), Some(2));
        assert_eq!(step(&[], None, true), None);
    }

    #[test]
    fn find_wraps_from_the_highlight() {
        let e = |l| item(None, l, |_, _| {});
        let entries = [e("Haiku"), Entry::Sep, e("High"), e("Max")];
        assert_eq!(find(&entries, "h", 0), Some(0));
        assert_eq!(find(&entries, "h", 1), Some(2));
        assert_eq!(find(&entries, "h", 3), Some(0));
        assert_eq!(find(&entries, "hi", 0), Some(2));
        assert_eq!(find(&entries, "x", 0), None);
    }
}

#[cfg(all(test, feature = "test-support"))]
mod interaction_tests;
