mod agent;
mod app;
mod chat;
mod git;
mod pr;
mod state;

use gpui_kit::component::{Theme, ThemeConfig, ThemeRegistry, ThemeSet, TitleBar};
use gpui_kit::*;
use std::borrow::Cow;
use std::rc::Rc;

actions!(
    shark,
    [
        TogglePalette,
        NewWorkspace,
        ArchiveWorkspace,
        ForceArchiveWorkspace,
        AddRepo,
        StopAgent,
        FocusSidebar,
        FocusMain,
        ShowChat,
        ShowDiff,
        CreatePrs,
        OpenPrs,
        SelectNext,
        SelectPrev,
        SelectFirst,
        SelectLast,
        PageDown,
        PageUp,
        Confirm,
        FocusComposer,
        FocusTranscript,
        NextHunk,
        PrevHunk,
        NextFile,
        PrevFile,
        ToggleDiffMode,
        RefreshDiff,
        PickModel,
    ]
);

/// Switch the open workspace to `chat::MODELS[n]` without opening the menu.
#[derive(Clone, PartialEq, Debug, serde::Deserialize, Action)]
#[action(namespace = shark, no_json)]
pub struct SetModel(pub usize);

/// Apps launched from Finder get a minimal PATH; borrow the login shell's so claude/gh/git resolve.
fn fix_path() {
    let shell = std::env::var("SHELL").unwrap_or("/bin/zsh".into());
    if let Ok(out) = std::process::Command::new(shell)
        .args(["-lic", "printf '__P__%s' \"$PATH\""])
        .output()
        && let Some(path) = String::from_utf8_lossy(&out.stdout).split("__P__").nth(1)
    {
        // SAFETY: called first thing in main, before any other threads exist.
        unsafe { std::env::set_var("PATH", path.trim()) };
    }
}

fn bind_keys(cx: &mut App) {
    const NAV: &str = "Sidebar || Transcript || DiffPane";
    const LIST: &str = "Sidebar || Transcript";
    cx.bind_keys([
        KeyBinding::new("cmd-k", TogglePalette, None),
        KeyBinding::new("cmd-shift-p", TogglePalette, None),
        KeyBinding::new("cmd-n", NewWorkspace, None),
        KeyBinding::new("ctrl-c", StopAgent, None),
        KeyBinding::new("ctrl-w h", FocusSidebar, None),
        KeyBinding::new("ctrl-w l", FocusMain, None),
        KeyBinding::new("j", SelectNext, Some(LIST)),
        KeyBinding::new("k", SelectPrev, Some(LIST)),
        KeyBinding::new("g g", SelectFirst, Some(LIST)),
        KeyBinding::new("shift-g", SelectLast, Some(LIST)),
        KeyBinding::new("ctrl-d", PageDown, Some(LIST)),
        KeyBinding::new("ctrl-u", PageUp, Some(LIST)),
        KeyBinding::new("enter", Confirm, Some(LIST)),
        KeyBinding::new("o", Confirm, Some("Transcript")),
        KeyBinding::new("i", FocusComposer, Some("Transcript")),
        KeyBinding::new("a", FocusComposer, Some("Transcript")),
        KeyBinding::new("escape", FocusTranscript, Some("Composer")),
        KeyBinding::new("space space", TogglePalette, Some(NAV)),
        KeyBinding::new("space w n", NewWorkspace, Some(NAV)),
        KeyBinding::new("space w d", ArchiveWorkspace, Some(NAV)),
        KeyBinding::new("space w shift-d", ForceArchiveWorkspace, Some(NAV)),
        KeyBinding::new("space r a", AddRepo, Some(NAV)),
        KeyBinding::new("space c", ShowChat, Some(NAV)),
        KeyBinding::new("space d", ShowDiff, Some(NAV)),
        KeyBinding::new("space p r", CreatePrs, Some(NAV)),
        KeyBinding::new("space p o", OpenPrs, Some(NAV)),
        KeyBinding::new("cmd-/", PickModel, None),
        KeyBinding::new("space m m", PickModel, Some(NAV)),
        KeyBinding::new("] c", NextHunk, Some("DiffPane")),
        KeyBinding::new("[ c", PrevHunk, Some("DiffPane")),
        KeyBinding::new("}", NextFile, Some("DiffPane")),
        KeyBinding::new("{", PrevFile, Some("DiffPane")),
        KeyBinding::new("s", ToggleDiffMode, Some("DiffPane")),
        KeyBinding::new("r", RefreshDiff, Some("DiffPane")),
    ]);
    // Direct picks, numbered like the model menu: ctrl-cmd-1 / space m 1 is the first row.
    cx.bind_keys((0..chat::MODELS.len()).flat_map(|i| {
        [
            KeyBinding::new(&format!("ctrl-cmd-{}", i + 1), SetModel(i), None),
            KeyBinding::new(&format!("space m {}", i + 1), SetModel(i), Some(NAV)),
        ]
    }));
    // The Diff component's scroll actions are private; bind vim keys to them by name.
    let diff = Rc::new(KeyBindingContextPredicate::parse("Diff").unwrap());
    let vim = [
        ("j", "diff::ScrollDown"),
        ("k", "diff::ScrollUp"),
        ("h", "diff::ScrollLeft"),
        ("l", "diff::ScrollRight"),
        ("ctrl-d", "diff::PageDown"),
        ("ctrl-u", "diff::PageUp"),
        ("g g", "diff::First"),
        ("shift-g", "diff::Last"),
    ];
    let bindings: Vec<KeyBinding> = vim
        .iter()
        .filter_map(|(k, name)| {
            let action = cx.build_action(name, None).ok()?;
            KeyBinding::load(
                k,
                action,
                Some(diff.clone()),
                false,
                None,
                &DummyKeyboardMapper,
            )
            .ok()
        })
        .collect();
    cx.bind_keys(bindings);
}

const THEMES: &str = include_str!("../assets/themes/linear.json");

/// Embedded so the UI looks the same on every Mac; both are OFL (licenses next to the files).
const FONTS: &[&[u8]] = &[
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../assets/fonts/Inter-Bold.ttf"),
    include_bytes!("../assets/fonts/Inter-Italic.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMonoNL-Regular.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMonoNL-Bold.ttf"),
];

fn linear_themes() -> Vec<ThemeConfig> {
    serde_json::from_str::<ThemeSet>(THEMES)
        .expect("assets/themes/linear.json")
        .themes
}

/// Installs the Linear light and dark themes and follows the system appearance.
fn install_theme(cx: &mut App) {
    let fonts = FONTS.iter().map(|f| Cow::Borrowed(*f)).collect();
    cx.text_system().add_fonts(fonts).expect("embedded fonts");
    let registry = ThemeRegistry::global(cx);
    let themes: Vec<Rc<ThemeConfig>> = linear_themes()
        .into_iter()
        .map(|mut t| {
            // Syntax colors aren't restyled; borrow the stock ones of the same mode.
            let stock = match t.mode.is_dark() {
                true => registry.default_dark_theme(),
                false => registry.default_light_theme(),
            };
            t.highlight = stock.highlight.clone();
            Rc::new(t)
        })
        .collect();
    Theme::update(cx, |theme| {
        themes.iter().for_each(|t| theme.apply_config(t))
    });
    Theme::sync_system_appearance(None, cx);
}

/// `cargo run` isn't a bundle, so there's no Info.plist icon; set the Dock icon directly.
fn set_dock_icon() {
    use objc2::{AllocAnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;
    let mtm = MainThreadMarker::new().expect("main thread");
    let data = NSData::with_bytes(include_bytes!("../assets/icon.png"));
    let image = NSImage::initWithData(NSImage::alloc(), &data).expect("assets/icon.png");
    unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image)) };
}

fn main() {
    fix_path();
    application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(|cx| {
            set_dock_icon();
            gpui_kit::init(cx);
            install_theme(cx);
            bind_keys(cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1280.), px(820.)), cx)),
                titlebar: Some(TitleBar::title_bar_options()),
                ..Default::default()
            };
            open_window(options, cx, |window, cx| {
                window
                    .observe_window_appearance(|window, cx| {
                        Theme::sync_system_appearance(Some(window), cx)
                    })
                    .detach();
                cx.new(|cx| app::Shark::new(window, cx))
            })
            .unwrap();
            cx.activate(true);
        });
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::ThemeMode;

    /// A typo in a key is silently ignored by serde, so check values that matter actually land.
    #[test]
    fn linear_themes_parse() {
        let themes = super::linear_themes();
        let modes: Vec<ThemeMode> = themes.iter().map(|t| t.mode).collect();
        assert_eq!(modes, [ThemeMode::Dark, ThemeMode::Light]);
        for t in &themes {
            let c = &t.colors;
            for key in [
                &c.background,
                &c.foreground,
                &c.muted_foreground,
                &c.sidebar,
                &c.primary,
                &c.link,
            ] {
                assert!(key.is_some(), "{} is missing a core color", t.name);
            }
            assert_eq!(t.font_family.as_deref(), Some("Inter"));
        }
    }
}
