//! The macOS menu bar. Items dispatch existing actions; shortcuts come from the keymap.

use crate::*;
use gpui_kit::component::input;

actions!(
    shark,
    [About, Quit, Hide, HideOthers, ShowAll, Minimize, Zoom]
);

/// Call after `bind_keys`: the menu reads its shortcut labels from the keymap.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
    ]);
    // Global, so they work with no window or while a dialog has focus. Quit is in `main`.
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &About, cx| in_window(cx, crate::settings::about));
    cx.on_action(|_: &Minimize, cx| in_window(cx, |w, _| w.minimize_window()));
    cx.on_action(|_: &Zoom, cx| in_window(cx, |w, _| w.zoom_window()));
    cx.set_menus(menus());
}

/// Deferred: action handlers run inside the window's update, which can't be re-entered.
fn in_window(cx: &mut App, f: impl FnOnce(&mut Window, &mut App) + 'static) {
    if let Some(window) = cx.active_window() {
        cx.defer(move |cx| {
            window.update(cx, |_, w, cx| f(w, cx)).ok();
        });
    }
}

fn menu(name: &'static str, items: Vec<MenuItem>) -> Menu {
    Menu {
        name: name.into(),
        items,
        disabled: false,
    }
}

pub fn menus() -> Vec<Menu> {
    let sep = MenuItem::separator;
    vec![
        menu(
            "Shark",
            vec![
                MenuItem::action("About Shark", About),
                sep(),
                MenuItem::action("Settings…", OpenSettings),
                sep(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                sep(),
                MenuItem::action("Hide Shark", Hide),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::action("Show All", ShowAll),
                sep(),
                MenuItem::action("Quit Shark", Quit),
            ],
        ),
        menu(
            "File",
            vec![
                MenuItem::action("New Workspace", NewWorkspace),
                MenuItem::action("New Chat", NewChat),
                MenuItem::action("Open File…", OpenAnyFile),
                sep(),
                MenuItem::action("Add Repo…", AddRepo),
                MenuItem::action("Add Repo to Workspace…", AddRepoToWorkspace),
                sep(),
                MenuItem::action("Save", SaveFile),
                MenuItem::action("Close Chat", CloseChat),
                MenuItem::action("Close File", CloseFile),
                MenuItem::action("Reopen Closed Chat", ReopenChat),
                sep(),
                MenuItem::action("Archive Workspace", ArchiveWorkspace),
            ],
        ),
        menu(
            "Edit",
            vec![
                MenuItem::os_action("Undo", input::Undo, OsAction::Undo),
                MenuItem::os_action("Redo", input::Redo, OsAction::Redo),
                sep(),
                MenuItem::os_action("Cut", input::Cut, OsAction::Cut),
                MenuItem::os_action("Copy", input::Copy, OsAction::Copy),
                MenuItem::os_action("Paste", input::Paste, OsAction::Paste),
                MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll),
            ],
        ),
        menu(
            "View",
            vec![
                MenuItem::action("Command Palette", TogglePalette),
                sep(),
                MenuItem::action("Toggle Sidebar", ToggleSidebar),
                MenuItem::action("Toggle File Tree", ToggleFileTree),
                sep(),
                MenuItem::action("Show Chat", ShowChat),
                MenuItem::action("Show Diff", ShowDiff),
                MenuItem::action("Toggle Plan", TogglePlan),
                sep(),
                MenuItem::action("Toggle Vim Mode", ToggleVim),
                MenuItem::action("Change Model…", PickModel),
            ],
        ),
        menu(
            "Window",
            vec![
                MenuItem::action("Minimize", Minimize),
                MenuItem::action("Zoom", Zoom),
                sep(),
                MenuItem::action("Next Chat", NextChat),
                MenuItem::action("Previous Chat", PrevChat),
                MenuItem::action("Next Needing Attention", NextAttention),
            ],
        ),
    ]
}
