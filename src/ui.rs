//! Shared UI styling and dialogs used by the host and workspace features.
pub(crate) mod pokemon_images;
use crate::state::Mode;
use crate::{PageDown, PageUp, SelectFirst, SelectLast, SelectNext, SelectPrev};
use gpui_kit::assets::IconName;
use gpui_kit::base::{ResizeHandleRenderer, ResizeHandleState};
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::command::CommandItem;
use gpui_kit::component::dialog::{Dialog, DialogEntrance};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::resize_handle_appearance;
use gpui_kit::component::{Selectable as _, Sizable as _, Theme, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{rc::Rc, sync::Arc};

/// Inter disambiguation (ss02 + cv05/cv08/zero): serifed I, tailed l, slashed 0 for paths and hashes.
/// Dialogs render outside this view's tree, so they apply it again.
pub(crate) fn inter_features() -> FontFeatures {
    FontFeatures(Arc::new(
        ["ss02", "cv05", "cv08", "zero"]
            .map(|f| (f.to_string(), 1))
            .to_vec(),
    ))
}

/// Every `open_dialog` starts here: dialogs render outside the view's tree (see `inter_features`),
/// and none of them animates in.
pub(crate) fn dialog(d: Dialog) -> Dialog {
    d.font_features(inter_features())
        .entrance(DialogEntrance::None)
}

/// Hint text below the muted level; gpui-kit has no token for it. Never use it for content.
/// `docs/themes.py` checks the same blend against the contrast floor.
pub(crate) fn faint(theme: &Theme) -> Hsla {
    theme.muted_foreground.blend(theme.background.opacity(0.4))
}

pub(crate) fn notify_err(window: &mut Window, cx: &mut App, e: impl Into<SharedString>) {
    window.push_notification(Notification::error(e), cx);
}

pub(crate) fn kbd(keys: &str) -> Kbd {
    Kbd::new(Keystroke::parse(keys).unwrap())
}

/// Keys (space-separated) and what they do, for a hint row.
pub(crate) fn hint(keys: &str, label: &'static str) -> Div {
    h_flex()
        .gap_1()
        .children(keys.split(' ').map(kbd))
        .child(label)
}

/// The "s" of `n` things.
pub(crate) fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// A Danger-button alert; `ok` runs on the entity once confirmed.
pub(crate) fn confirm<T: 'static>(
    title: String,
    description: &'static str,
    ok_text: &'static str,
    ok: impl Fn(&mut T, &mut Window, &mut Context<T>) + 'static,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let (target, ok) = (cx.entity().downgrade(), Rc::new(ok));
    window.open_alert_dialog(cx, move |d, _, _| {
        let (target, ok) = (target.clone(), ok.clone());
        d.font_features(inter_features())
            .entrance(DialogEntrance::None)
            .title(title.clone())
            .description(description)
            .ok_text(ok_text)
            .ok_variant(ButtonVariant::Danger)
            .show_cancel(true)
            .on_ok(move |_, window, cx| {
                // Restore focus before the action can close or replace its original pane.
                window.close_dialog(cx);
                target.update(cx, |s, cx| ok(s, window, cx)).ok();
                false
            })
    });
}

/// A side-pane row's hover, and the pill for its open or cursor row: `foreground` over the
/// `sidebar` color. Light mode takes less, so muted and status text keep their floors on the pill;
/// `docs/themes.py` checks the same blends.
pub(crate) fn side_hover(theme: &Theme) -> Hsla {
    theme
        .foreground
        .alpha(if theme.is_dark() { 0.07 } else { 0.03 })
}

pub(crate) fn side_on(theme: &Theme) -> Hsla {
    theme
        .foreground
        .alpha(if theme.is_dark() { 0.11 } else { 0.05 })
}

/// A row in either side pane: 30px plus 2 below (a 32px pitch), its content 16px in from the
/// pane edge. `cursor_row` (or the caller) adds the hover.
pub(crate) fn side_row(theme: &Theme) -> Div {
    h_flex()
        .h(px(30.))
        .flex_none()
        .mx_2()
        .mb_0p5()
        .border_1()
        .border_color(theme.transparent)
        .px(px(7.))
        .gap_2()
        .rounded(theme.radius)
        .text_sm()
        .text_color(theme.muted_foreground)
}

/// A side pane's group header: "Workspaces", "Repos", "Files".
pub(crate) fn side_header(theme: &Theme, label: &'static str) -> Div {
    div()
        .px_4()
        .pt_4()
        .pb_1()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.muted_foreground)
        .child(label)
}

/// A side-pane row's state: the open item (`on`) gets the grey pill and `foreground`; the
/// cursor gets the pill plus a `primary` ring, only while its pane has focus. Other rows hover.
pub(crate) fn cursor_row<E: Styled + InteractiveElement>(
    e: E,
    on: bool,
    cursor: bool,
    focused: bool,
    theme: &Theme,
) -> E {
    let (pill, hover) = (side_on(theme), side_hover(theme));
    let e = if on {
        e.text_color(theme.foreground)
    } else {
        e
    };
    match (on, cursor && focused) {
        (_, true) => e.bg(pill).border_color(theme.primary),
        (true, false) => e.bg(pill),
        (false, false) => e.hover(move |d| d.bg(hover)),
    }
}

/// `cursor_row` inside the island (the Home list): `list_hover`, and while the list has focus
/// `list_active` plus a `primary` ring, since the tint alone is too subtle to find the cursor.
pub(crate) fn list_cursor_row<E: Styled + InteractiveElement>(
    e: E,
    selected: bool,
    focused: bool,
    theme: &Theme,
) -> E {
    let e = (e.border_1())
        .border_color(theme.transparent)
        .hover(|d| d.bg(theme.list_hover));
    match (selected, focused) {
        (true, true) => e.bg(theme.list_active).border_color(theme.primary),
        (true, false) => e.bg(theme.list_hover),
        (false, _) => e,
    }
}

/// A titlebar tab, raised above the window chrome.
pub(crate) fn pill(id: impl Into<SharedString>, on: bool, theme: &Theme) -> Stateful<Div> {
    h_flex()
        .id(id.into())
        .role(Role::Tab)
        .aria_selected(on)
        .h(px(28.))
        .w(px(200.))
        .min_w(px(100.))
        .flex_shrink_1()
        .relative()
        .overflow_hidden()
        .group("titlebar-tab")
        .gap_1p5()
        .rounded(theme.radius)
        .text_sm()
        .text_color(if on {
            theme.foreground
        } else {
            theme.sidebar_foreground
        })
        .bg(if on {
            theme.secondary
        } else {
            theme.background
        })
        .hover(|d| d.bg(theme.secondary_hover).text_color(theme.foreground))
}

/// Leave the left side selectable even when a tab is narrower than its close button.
pub(crate) fn tab_close(button: Button, theme: &Theme) -> Div {
    h_flex()
        .absolute()
        .top(px(2.))
        .left(px(28.))
        .right(px(2.))
        .h(px(24.))
        .justify_end()
        .overflow_hidden()
        .invisible()
        .group_hover("titlebar-tab", |s| s.visible())
        .child(
            div()
                .flex_none()
                .bg(theme.secondary_hover)
                .rounded(theme.radius)
                .child(button),
        )
}

/// An icon control in the window chrome.
pub(crate) fn side_toggle(id: &'static str, icon: IconName, tip: &'static str) -> Button {
    Button::new(id)
        .ghost()
        .icon(icon)
        .w(px(32.))
        .h(px(28.))
        .accessibility_label(tip)
        .tooltip(tip)
}

/// The overflow overlay follows the tab surface, including hover.
pub(crate) fn tab_label(label: SharedString, on: bool, theme: &Theme) -> Div {
    crate::overflow_fade::label(
        label.replace(['\n', '\r'], " ").into(),
        if on {
            theme.secondary
        } else {
            theme.background
        },
        Some(("titlebar-tab", theme.secondary_hover)),
    )
}

/// A side pane's drag edge: no hairline at rest, since the island's border already marks it;
/// the usual line and pill while hovered or dragged.
pub(crate) fn quiet_edge() -> ResizeHandleRenderer {
    let usual = resize_handle_appearance();
    Rc::new(move |h, w, cx| match h.state() {
        ResizeHandleState::Idle => Some(div().into_any_element()),
        _ => usual(h, w, cx),
    })
}
pub(crate) fn copy(text: String, window: &mut Window, cx: &mut App) {
    let msg = format!("Copied {text}");
    copy_with(text, msg, window, cx);
}

/// Puts `text` on the clipboard and toasts `msg`.
pub(crate) fn copy_with(
    text: String,
    msg: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut App,
) {
    window.push_notification(msg.into(), cx);
    cx.write_to_clipboard(ClipboardItem::new_string(text));
}

/// Shown when AppKit has no icon for the app.
pub(crate) fn app_icon(app: &crate::apps::OpenApp) -> IconName {
    match app.kind {
        crate::apps::Kind::Terminal => IconName::SquareTerminal,
        crate::apps::Kind::Folder => IconName::AppWindow,
        crate::apps::Kind::Git => IconName::GitBranch,
        crate::apps::Kind::Xcode => IconName::FileCode,
    }
}

/// A ghost button that shows its selection, so tabs and mode are tab stops with enter/space.
pub(crate) fn option_pill(label: &'static str, icon: Option<IconName>, active: bool) -> Button {
    Button::new(label)
        .ghost()
        .small()
        .label(label)
        .when_some(icon, |b, i| b.icon(i))
        .selected(active)
}

/// The System, Light and Dark pills, each with its mode; the caller adds `on_click`.
pub(crate) fn mode_pills(current: Mode) -> impl Iterator<Item = (Mode, Button)> {
    [
        (Mode::System, "System", IconName::Monitor),
        (Mode::Light, "Light", IconName::Sun),
        (Mode::Dark, "Dark", IconName::Moon),
    ]
    .into_iter()
    .map(move |(m, label, icon)| (m, option_pill(label, Some(icon), m == current)))
}

/// The small X that closes or dismisses something.
pub(crate) fn close_button(id: &'static str, tooltip: &'static str) -> Button {
    Button::new(id)
        .ghost()
        .xsmall()
        .icon(IconName::X)
        .tooltip(tooltip)
}

pub(crate) fn section(theme: &Theme, label: &'static str) -> impl IntoElement {
    div()
        .text_xs()
        .text_color(theme.muted_foreground)
        .font_weight(FontWeight::MEDIUM)
        .child(label)
}

pub(crate) fn card(theme: &Theme) -> Div {
    v_flex()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
        .bg(theme.secondary)
}

pub(crate) fn caption(theme: &Theme, label: &'static str, desc: &'static str) -> Div {
    v_flex().child(div().text_sm().child(label)).child(
        div()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(desc),
    )
}

/// Label and description on the left, the control on the right (Linear's settings rows).
pub(crate) fn row(
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
        .child(caption(theme, label, desc).flex_1().min_w_0())
        .child(
            div()
                .w(px(240.))
                .flex_none()
                .flex()
                .justify_end()
                .child(control),
        )
}

/// A command-menu row with an image icon (an app's or a file's) before its label.
pub(crate) fn image_item(label: SharedString, icon: Arc<Image>) -> CommandItem {
    CommandItem::new().label(label.clone()).child(move |_, _| {
        h_flex()
            .flex_1()
            .gap_2()
            .items_center()
            .child(img(icon.clone()).size_4().flex_none())
            .child(div().flex_1().min_w_0().truncate().child(label.clone()))
    })
}

/// A move of a list cursor, as the select and page actions ask for it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Nav {
    Next,
    Prev,
    First,
    Last,
    PageDown,
    PageUp,
}

impl Nav {
    /// Where the cursor at `at` of `len` rows goes, clamped to the list; `page` rows per page.
    pub(crate) fn to(self, at: usize, len: usize, page: usize) -> usize {
        let to = match self {
            Nav::Next => at + 1,
            Nav::Prev => at.saturating_sub(1),
            Nav::First => 0,
            Nav::Last => usize::MAX,
            Nav::PageDown => at + page,
            Nav::PageUp => at.saturating_sub(page),
        };
        to.min(len.saturating_sub(1))
    }
}

/// Wires the six list-navigation actions on `el` to `go`; without `pages` the page keys
/// pass on to whatever is above.
pub(crate) fn nav_actions<T: 'static, E: InteractiveElement>(
    el: E,
    pages: bool,
    cx: &Context<T>,
    go: fn(&mut T, Nav, &mut Context<T>),
) -> E {
    el.on_action(cx.listener(move |t, _: &SelectNext, _, cx| go(t, Nav::Next, cx)))
        .on_action(cx.listener(move |t, _: &SelectPrev, _, cx| go(t, Nav::Prev, cx)))
        .on_action(cx.listener(move |t, _: &SelectFirst, _, cx| go(t, Nav::First, cx)))
        .on_action(cx.listener(move |t, _: &SelectLast, _, cx| go(t, Nav::Last, cx)))
        .on_action(cx.listener(move |t, _: &PageDown, _, cx| match pages {
            true => go(t, Nav::PageDown, cx),
            false => cx.propagate(),
        }))
        .on_action(cx.listener(move |t, _: &PageUp, _, cx| match pages {
            true => go(t, Nav::PageUp, cx),
            false => cx.propagate(),
        }))
}

#[cfg(test)]
mod nav_tests {
    use super::Nav;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro

    #[test]
    fn nav_clamps_to_the_list() {
        assert_eq!(Nav::Next.to(2, 3, 10), 2);
        assert_eq!(Nav::Next.to(0, 3, 10), 1);
        assert_eq!(Nav::Prev.to(0, 3, 10), 0);
        assert_eq!(Nav::First.to(2, 3, 10), 0);
        assert_eq!(Nav::Last.to(0, 3, 10), 2);
        assert_eq!(Nav::PageDown.to(1, 30, 10), 11);
        assert_eq!(Nav::PageDown.to(25, 30, 10), 29);
        assert_eq!(Nav::PageUp.to(4, 30, 10), 0);
        assert_eq!(Nav::Last.to(0, 0, 10), 0, "an empty list stays at 0");
    }
}
