mod agent;
mod app;
mod chat;
mod git;
mod pr;
mod state;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;
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
    ]
);

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
        KeyBinding::new("] c", NextHunk, Some("DiffPane")),
        KeyBinding::new("[ c", PrevHunk, Some("DiffPane")),
        KeyBinding::new("}", NextFile, Some("DiffPane")),
        KeyBinding::new("{", PrevFile, Some("DiffPane")),
        KeyBinding::new("s", ToggleDiffMode, Some("DiffPane")),
        KeyBinding::new("r", RefreshDiff, Some("DiffPane")),
    ]);
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

fn main() {
    fix_path();
    application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            bind_keys(cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1280.), px(820.)), cx)),
                ..Default::default()
            };
            open_window(options, cx, |window, cx| {
                cx.new(|cx| app::Shark::new(window, cx))
            })
            .unwrap();
            cx.activate(true);
        });
}
