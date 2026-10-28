//! Shared UI styling and dialogs used by the host and workspace features.
use gpui_kit::assets::IconName;
use gpui_kit::base::{ResizeHandleRenderer, ResizeHandleState};
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::resize_handle_appearance;
use gpui_kit::component::{Theme, WindowExt, h_flex};
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
    window.push_notification(format!("Copied {text}"), cx);
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
