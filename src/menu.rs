//! Right-click menus drawn after Linear's: 28px rows in an inset rounded highlight,
//! muted 14px icons, 1px separators, metadata right-aligned in the muted colour.
//! gpui-kit's `PopupMenu` hard-codes its row height, 2px separators and icon colour.
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme, Icon, Sizable as _, Theme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
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

impl Menu {
    /// Starts the highlight on `ix`, e.g. the current choice of a picker.
    pub(crate) fn select(&mut self, ix: usize) {
        self.sel = Some(ix);
    }

    /// The first row whose label starts with what was typed, ignoring case.
    fn jump(&mut self, ch: &str) -> bool {
        self.typed.push_str(&ch.to_lowercase());
        let find = |typed: &str| {
            self.entries.iter().position(|e| match e {
                Entry::Item { label, .. } | Entry::Sub { label, .. } => {
                    label.to_lowercase().starts_with(typed)
                }
                Entry::Sep => false,
            })
        };
        let hit = find(&self.typed).or_else(|| {
            self.typed = ch.to_lowercase();
            find(&self.typed)
        });
        if let Some(ix) = hit {
            self.sel = Some(ix);
            self.sub = None;
            self.sub_sel = None;
        }
        hit.is_some()
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(f) = self.prev.take() {
            f.focus(window, cx);
        }
        cx.emit(DismissEvent);
    }

    fn sub_entries(&self) -> &[Entry] {
        match self.sub.map(|ix| &self.entries[ix]) {
            Some(Entry::Sub { entries, .. }) => entries,
            _ => &[],
        }
    }

    /// Runs an item, or opens a submenu with its first row highlighted.
    fn activate(&mut self, ix: usize, in_sub: bool, window: &mut Window, cx: &mut Context<Self>) {
        let list = if in_sub {
            self.sub_entries()
        } else {
            &self.entries
        };
        match &list[ix] {
            Entry::Item { on_click, .. } => {
                let f = on_click.clone();
                self.close(window, cx);
                f(window, cx);
            }
            Entry::Sub { entries, .. } => {
                self.sub_sel = step(entries, None, true);
                self.sub = Some(ix);
                cx.notify();
            }
            Entry::Sep => {}
        }
    }

    fn hover(&mut self, ix: usize, x: Pixels, cx: &mut Context<Self>) {
        // Heading right is heading into the open submenu: crossing a sibling row on
        // the way mustn't swap it. ponytail: x direction only, not Radix's triangle.
        let toward_sub = self.sub.is_some_and(|s| s != ix) && x > self.last_x;
        self.last_x = x;
        if toward_sub {
            return;
        }
        let sub = matches!(self.entries[ix], Entry::Sub { .. }).then_some(ix);
        if self.sel == Some(ix) && self.sub == sub {
            return;
        }
        self.sel = Some(ix);
        self.sub = sub;
        self.sub_sel = None;
        cx.notify();
    }

    fn key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let in_sub = self.sub_sel.is_some();
        match e.keystroke.key.as_str() {
            "escape" | "left" if in_sub => {
                self.sub = None;
                self.sub_sel = None;
            }
            "escape" => self.close(window, cx),
            "up" | "down" if in_sub => {
                self.sub_sel = step(self.sub_entries(), self.sub_sel, e.keystroke.key == "down")
            }
            "up" | "down" => {
                self.sel = step(&self.entries, self.sel, e.keystroke.key == "down");
                self.sub = None;
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
            k if !in_sub
                && k.chars().count() == 1
                && !e.keystroke.modifiers.modified()
                && self.jump(k) => {}
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
        cx: &Context<Self>,
    ) -> Div {
        let theme = cx.theme();
        let icons = entries.iter().any(|e| {
            matches!(
                e,
                Entry::Item { icon: Some(_), .. }
                    | Entry::Item { image: Some(_), .. }
                    | Entry::Sub { .. }
            )
        });
        let rows = entries.iter().enumerate().map(|(ix, e)| {
            let (icon, image, label, right) = match e {
                Entry::Sep => {
                    return div()
                        .h(px(1.))
                        .my(px(4.))
                        .mx(px(-4.))
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
                } => (
                    *icon,
                    image.clone(),
                    label,
                    Some(
                        h_flex()
                            .gap(px(8.))
                            .children(hint.as_ref().map(|h| {
                                div()
                                    .max_w(px(180.))
                                    .truncate()
                                    .text_color(theme.muted_foreground)
                                    .child(h.clone())
                            }))
                            .when(*checked, |d| {
                                d.child(
                                    Icon::new(IconName::Check)
                                        .with_size(px(14.))
                                        .text_color(theme.muted_foreground),
                                )
                            })
                            .into_any_element(),
                    ),
                ),
                Entry::Sub {
                    icon,
                    image,
                    label,
                    hint,
                    ..
                } => (
                    Some(*icon),
                    image.clone(),
                    label,
                    Some(
                        h_flex()
                            .gap(px(8.))
                            .text_color(theme.muted_foreground)
                            .children(hint.as_ref().map(|h| div().max_w(px(180.)).truncate().child(h.clone())))
                            .child(Icon::new(IconName::ChevronRight).with_size(px(12.)))
                            .into_any_element(),
                    ),
                ),
            };
            let open = !in_sub && self.sub == Some(ix);
            h_flex()
                .id(ix)
                .relative()
                .h(px(28.))
                .px(px(8.))
                .gap(px(8.))
                .rounded(px(4.))
                .when(sel == Some(ix) || open, |d| d.bg(highlight(theme)))
                .when(icons, |d| {
                    d.child(match (image, icon) {
                        // App icons have a margin built in; 16px matches a 14px glyph.
                        (Some(app), _) => img(app)
                            .size(px(16.))
                            .mx(px(-1.))
                            .flex_none()
                            .into_any_element(),
                        (None, Some(i)) => Icon::new(i)
                            .with_size(px(14.))
                            .text_color(theme.muted_foreground)
                            .into_any_element(),
                        (None, None) => div().size(px(14.)).flex_none().into_any_element(),
                    })
                })
                .child(div().flex_1().min_w_0().truncate().child(label.clone()))
                .children(right)
                .on_mouse_move(cx.listener(move |this, e: &MouseMoveEvent, _, cx| {
                    if in_sub {
                        if this.sub_sel != Some(ix) {
                            this.sub_sel = Some(ix);
                            cx.notify();
                        }
                    } else {
                        this.hover(ix, e.position.x, cx);
                    }
                }))
                .on_click(cx.listener(move |this, _, w, cx| this.activate(ix, in_sub, w, cx)))
                .when(open, |d| {
                    let sub = self.panel(self.sub_entries(), self.sub_sel, true, cx);
                    // A menu hanging from the window's right side opens its submenus leftward.
                    let leftward = matches!(self.corner, Anchor::TopRight | Anchor::BottomRight);
                    d.child(
                        div()
                            .absolute()
                            .top(px(-4.))
                            .when(!leftward, |d| d.left_full().pl(px(4.)))
                            .when(leftward, |d| d.right_full().pr(px(4.)))
                            // Deferred above the main panel, so a submenu the window
                            // edge pushes back over it still gets the mouse.
                            .child(
                                deferred(
                                    anchored()
                                        .anchor(if leftward { Anchor::TopRight } else { Anchor::TopLeft })
                                        .snap_to_window_with_margin(px(8.))
                                        .child(sub),
                                )
                                    .with_priority(POPUP_PRIORITY + 1),
                            ),
                    )
                })
                .into_any_element()
        });
        surface(theme).children(rows.collect::<Vec<_>>())
    }
}

fn surface(theme: &Theme) -> Div {
    chrome(theme).occlude().min_w(px(200.)).max_w(px(340.))
}

/// The look every floating menu shares (this one and the composer's completions): `popover`,
/// 4px inset, 8px corners, and a translucent ring instead of a border, so the shadow shows
/// through the edge.
pub(crate) fn chrome(theme: &Theme) -> Div {
    let ink = hsla(0., 0., 0., if theme.is_dark() { 0.5 } else { 0.12 });
    v_flex()
        .p(px(4.))
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .text_sm()
        .rounded(theme.radius)
        .shadow(vec![
            BoxShadow::new(px(0.), px(0.), theme.foreground.alpha(0.1)).spread_radius(px(1.)),
            BoxShadow::new(px(0.), px(4.), ink).blur_radius(px(8.)),
            BoxShadow::new(px(0.), px(1.), ink.alpha(ink.a / 2.)).blur_radius(px(1.)),
        ])
}

/// The highlighted row: an inset 4px pill, the text colour unchanged.
pub(crate) fn highlight(theme: &Theme) -> Hsla {
    theme.foreground.alpha(0.07)
}

impl Render for Menu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A window-sized catcher under the menu: a click anywhere else only closes it.
        div()
            .occlude()
            .w(window.bounds().size.width)
            .h(window.bounds().size.height)
            .on_any_mouse_down(cx.listener(|this, _, w, cx| this.close(w, cx)))
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .child(
                anchored()
                    .position(self.pos)
                    .anchor(self.corner)
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        self.panel(&self.entries, self.sel, false, cx)
                            .track_focus(&self.focus)
                            .on_key_down(cx.listener(Self::key)),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin

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
}
