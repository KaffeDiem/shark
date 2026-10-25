//! Right-click menus drawn after Linear's: 28px rows in an inset rounded highlight,
//! muted 14px icons, 1px separators, metadata right-aligned in the muted colour.
//! gpui-kit's `PopupMenu` hard-codes its row height, 2px separators and icon colour.
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme, Icon, Sizable as _, Theme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::rc::Rc;

type OnClick = Rc<dyn Fn(&mut Window, &mut App)>;

pub(crate) enum Entry {
    Item {
        icon: Option<IconName>,
        label: SharedString,
        /// Right-aligned and muted: the branch a copy will copy, the path an open will open.
        hint: Option<SharedString>,
        on_click: OnClick,
    },
    Sub {
        icon: IconName,
        label: SharedString,
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
        label: label.into(),
        hint: None,
        on_click: Rc::new(on_click),
    }
}

pub(crate) fn sub(icon: IconName, label: impl Into<SharedString>, entries: Vec<Entry>) -> Entry {
    Entry::Sub {
        icon,
        label: label.into(),
        entries,
    }
}

impl Entry {
    pub(crate) fn hint(mut self, text: impl Into<SharedString>) -> Self {
        if let Entry::Item { hint, .. } = &mut self {
            *hint = Some(text.into());
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
        focus,
        prev,
        sel: None,
        sub: None,
        sub_sel: None,
        last_x: px(0.),
    })
}

pub(crate) fn layer(menu: Entity<Menu>) -> impl IntoElement {
    deferred(anchored().child(menu)).with_priority(POPUP_PRIORITY)
}

const POPUP_PRIORITY: usize = 100;

pub(crate) struct Menu {
    entries: Vec<Entry>,
    pos: Point<Pixels>,
    focus: FocusHandle,
    prev: Option<FocusHandle>,
    /// Highlighted row; the mouse and the arrow keys share it, as in Linear.
    sel: Option<usize>,
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
        let icons = entries
            .iter()
            .any(|e| matches!(e, Entry::Item { icon: Some(_), .. } | Entry::Sub { .. }));
        let rows = entries.iter().enumerate().map(|(ix, e)| {
            let (icon, label, right) = match e {
                Entry::Sep => {
                    return div()
                        .h(px(1.))
                        .my(px(4.))
                        .mx(px(-4.))
                        .bg(theme.foreground.alpha(0.08))
                        .into_any_element();
                }
                Entry::Item {
                    icon, label, hint, ..
                } => (
                    *icon,
                    label,
                    hint.as_ref().map(|h| {
                        div()
                            .max_w(px(180.))
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .child(h.clone())
                            .into_any_element()
                    }),
                ),
                Entry::Sub { icon, label, .. } => (
                    Some(*icon),
                    label,
                    Some(
                        Icon::new(IconName::ChevronRight)
                            .with_size(px(12.))
                            .text_color(theme.muted_foreground)
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
                .when(sel == Some(ix) || open, |d| {
                    d.bg(theme.foreground.alpha(0.07))
                })
                .when(icons, |d| {
                    d.child(match icon {
                        Some(i) => Icon::new(i)
                            .with_size(px(14.))
                            .text_color(theme.muted_foreground)
                            .into_any_element(),
                        None => div().size(px(14.)).flex_none().into_any_element(),
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
                    d.child(
                        div()
                            .absolute()
                            .top(px(-4.))
                            .left_full()
                            .pl(px(4.))
                            // Deferred above the main panel, so a submenu the window
                            // edge pushes back over it still gets the mouse.
                            .child(
                                deferred(anchored().snap_to_window_with_margin(px(8.)).child(sub))
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
    let ink = hsla(0., 0., 0., if theme.is_dark() { 0.5 } else { 0.12 });
    v_flex()
        .occlude()
        .p(px(4.))
        .min_w(px(200.))
        .max_w(px(340.))
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .text_size(px(13.))
        .rounded(px(8.))
        // A translucent ring instead of a border, so the shadow shows through the edge.
        .shadow(vec![
            BoxShadow::new(px(0.), px(0.), theme.foreground.alpha(0.1)).spread_radius(px(1.)),
            BoxShadow::new(px(0.), px(4.), ink).blur_radius(px(8.)),
            BoxShadow::new(px(0.), px(1.), ink.alpha(ink.a / 2.)).blur_radius(px(1.)),
        ])
}

impl Render for Menu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A window-sized catcher under the menu: a click anywhere else only closes it.
        div()
            .occlude()
            .w(window.bounds().size.width)
            .h(window.bounds().size.height)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, w, cx| this.close(w, cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, w, cx| this.close(w, cx)),
            )
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .child(
                anchored()
                    .position(self.pos)
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
