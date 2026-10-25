mod agent;
mod app;
mod attention;
mod chat;
mod codex;
#[cfg(test)]
mod codex_live;
mod complete;
mod editor;
mod files;
mod git;
mod menu;
mod plan;
mod pr;
mod prompts;
mod settings;
mod state;
mod sync;
mod vim;

use gpui_kit::component::TitleBar;
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
        AddRepoToWorkspace,
        StopAgent,
        FocusLeft,
        FocusRight,
        ShowChat,
        ShowDiff,
        CreatePrs,
        OpenPrs,
        CreateDraftPrs,
        MergeEarlier,
        MergeLater,
        GitNext,
        GitMenu,
        GitFetch,
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
        ToggleSidebar,
        ToggleFileTree,
        ToggleViewed,
        OpenFile,
        CollapseDir,
        ExpandDir,
        SaveFile,
        CloseFile,
        ToggleVim,
        RevealInFinder,
        OpenInTerminal,
        CopyPath,
        CopyBranch,
        ToggleRepo,
        SwitchField,
        SubmitForm,
        NextAttention,
        OpenSettings,
        EditQueued,
        DeleteQueued,
        MoveQueuedUp,
        MoveQueuedDown,
        FocusQueue,
        NewChat,
        CloseChat,
        ReopenChat,
        NextChat,
        PrevChat,
        AskSide,
        OpenSide,
        CloseSide,
        CopySide,
        InsertSide,
        NewSection,
        MoveToSection,
        RenameSection,
        SectionIcon,
        DeleteSection,
        CancelEdit,
    ]
);

// Plan mode: shift-tab, the plan pane (`space l v`) and its keys.
actions!(
    shark,
    [
        TogglePlanMode,
        TogglePlan,
        ApprovePlan,
        PlanOlder,
        PlanNewer,
        TogglePlanChanges
    ]
);

// Editor and diff review: `space f f`, and `c` / `shift-c` in the diff.
actions!(shark, [OpenAnyFile, AddComment, SendComments]);

/// Switch the open workspace to `chat::MODELS[n]` without opening the menu.
#[derive(Clone, PartialEq, Debug, serde::Deserialize, Action)]
#[action(namespace = shark, no_json)]
pub struct SetModel(pub usize);

/// Open the nth sidebar workspace (cmd-1 is the top one).
#[derive(Clone, PartialEq, Debug, serde::Deserialize, Action)]
#[action(namespace = shark, no_json)]
pub struct OpenWorkspace(pub usize);

/// Apps launched from Finder get a minimal PATH; borrow the login shell's so claude/gh/git resolve.
fn fix_path() {
    let shell = std::env::var("SHELL").unwrap_or("/bin/zsh".into());
    // Bounded: an rc file that blocks (pinentry, a network call) must not hang launch.
    let probe = ["-lic", "printf '__P__%s' \"$PATH\""];
    if let Ok(out) = git::cmd_bounded(std::path::Path::new("/"), &shell, &probe, 5)
        && let Some(path) = out.split("__P__").nth(1)
    {
        // SAFETY: called first thing in main; the only other thread is cmd_bounded's waiter,
        // which has already handed over the output and never reads the environment.
        unsafe { std::env::set_var("PATH", path.trim()) };
    }
}

fn bind_keys(cx: &mut App) {
    // VimNormal: the editor in Vim's Normal mode, where space types nothing.
    const NAV: &str =
        "Sidebar || Transcript || DiffPane || FileTree || Queue || PlanPane || VimNormal";
    const LIST: &str = "Sidebar || Transcript || FileTree || Queue";
    // The diff's line cursor (src/app/review.rs) moves like a list; h/l stay native there.
    const MOVE: &str = "Sidebar || Transcript || FileTree || Queue || DiffPane";
    cx.bind_keys([
        KeyBinding::new("cmd-k", TogglePalette, None),
        KeyBinding::new("cmd-shift-p", TogglePalette, None),
        KeyBinding::new("cmd-n", NewWorkspace, None),
        // Not in the editor: Vim users leave insert mode with ctrl-c.
        KeyBinding::new("ctrl-c", StopAgent, Some("!FileEditor")),
        KeyBinding::new("ctrl-w h", FocusLeft, None),
        KeyBinding::new("ctrl-w l", FocusRight, None),
        KeyBinding::new("cmd-b", ToggleSidebar, None),
        KeyBinding::new("space b", ToggleSidebar, Some(NAV)),
        KeyBinding::new("space t", ToggleFileTree, Some(NAV)),
        KeyBinding::new("space x", CloseFile, Some(NAV)),
        KeyBinding::new("ctrl-cmd-v", ToggleVim, None),
        // In the diff: the file last moved to with `}`/`{` or the tree.
        KeyBinding::new("v", ToggleViewed, Some("FileTree || DiffPane")),
        KeyBinding::new("o", OpenFile, Some("FileTree || DiffPane")),
        KeyBinding::new("h", CollapseDir, Some("FileTree")),
        KeyBinding::new("l", ExpandDir, Some("FileTree")),
        KeyBinding::new("cmd-s", SaveFile, Some("FileEditor")),
        KeyBinding::new("cmd-w", CloseFile, Some("FileEditor")),
        KeyBinding::new("cmd-t", NewChat, None),
        KeyBinding::new("cmd-w", CloseChat, None),
        KeyBinding::new("cmd-shift-t", ReopenChat, None),
        KeyBinding::new("cmd-shift-]", NextChat, None),
        KeyBinding::new("cmd-shift-[", PrevChat, None),
        KeyBinding::new("cmd-}", NextChat, None),
        KeyBinding::new("cmd-{", PrevChat, None),
        KeyBinding::new("ctrl-tab", NextChat, None),
        KeyBinding::new("ctrl-shift-tab", PrevChat, None),
        KeyBinding::new("g t", NextChat, Some(NAV)),
        KeyBinding::new("g shift-t", PrevChat, Some(NAV)),
        KeyBinding::new("cmd-shift-enter", AskSide, Some("Composer")),
        KeyBinding::new("space ?", OpenSide, Some(NAV)),
        KeyBinding::new("space shift-/", OpenSide, Some(NAV)),
        KeyBinding::new("escape", CloseSide, Some("SideAnswer")),
        KeyBinding::new("c", CopySide, Some("SideAnswer")),
        KeyBinding::new("enter", InsertSide, Some("SideAnswer")),
        KeyBinding::new("j", SelectNext, Some(MOVE)),
        KeyBinding::new("k", SelectPrev, Some(MOVE)),
        KeyBinding::new("g g", SelectFirst, Some(MOVE)),
        KeyBinding::new("shift-g", SelectLast, Some(MOVE)),
        KeyBinding::new("ctrl-d", PageDown, Some(MOVE)),
        KeyBinding::new("ctrl-u", PageUp, Some(MOVE)),
        KeyBinding::new("enter", Confirm, Some(LIST)),
        KeyBinding::new("o", Confirm, Some("Transcript")),
        KeyBinding::new("i", FocusComposer, Some("Transcript")),
        KeyBinding::new("a", FocusComposer, Some("Transcript")),
        KeyBinding::new("escape", FocusTranscript, Some("Composer")),
        KeyBinding::new("space q", FocusQueue, Some(NAV)),
        KeyBinding::new("up", SelectPrev, Some("Queue")),
        KeyBinding::new("down", SelectNext, Some("Queue")),
        KeyBinding::new("e", EditQueued, Some("Queue")),
        KeyBinding::new("d", DeleteQueued, Some("Queue")),
        KeyBinding::new("backspace", DeleteQueued, Some("Queue")),
        KeyBinding::new("shift-k", MoveQueuedUp, Some("Queue")),
        KeyBinding::new("shift-j", MoveQueuedDown, Some("Queue")),
        KeyBinding::new("alt-up", MoveQueuedUp, Some("Queue")),
        KeyBinding::new("alt-down", MoveQueuedDown, Some("Queue")),
        KeyBinding::new("escape", FocusComposer, Some("Queue")),
        KeyBinding::new("i", FocusComposer, Some("Queue")),
        KeyBinding::new("a", FocusComposer, Some("Queue")),
        KeyBinding::new("space space", TogglePalette, Some(NAV)),
        KeyBinding::new("space w n", NewWorkspace, Some(NAV)),
        KeyBinding::new("space w d", ArchiveWorkspace, Some(NAV)),
        KeyBinding::new("space w shift-d", ForceArchiveWorkspace, Some(NAV)),
        KeyBinding::new("space r a", AddRepo, Some(NAV)),
        KeyBinding::new("space w a", AddRepoToWorkspace, Some(NAV)),
        KeyBinding::new("space c", ShowChat, Some(NAV)),
        KeyBinding::new("space d", ShowDiff, Some(NAV)),
        KeyBinding::new("space p r", CreatePrs, Some(NAV)),
        KeyBinding::new("space p o", OpenPrs, Some(NAV)),
        KeyBinding::new("space p d", CreateDraftPrs, Some(NAV)),
        KeyBinding::new("space p k", MergeEarlier, Some(NAV)),
        KeyBinding::new("space p j", MergeLater, Some(NAV)),
        KeyBinding::new("space g g", GitNext, Some(NAV)),
        KeyBinding::new("space g m", GitMenu, Some(NAV)),
        KeyBinding::new("space g f", GitFetch, Some(NAV)),
        KeyBinding::new("space o f", RevealInFinder, Some(NAV)),
        KeyBinding::new("space o t", OpenInTerminal, Some(NAV)),
        KeyBinding::new("space y p", CopyPath, Some(NAV)),
        KeyBinding::new("space y b", CopyBranch, Some(NAV)),
        KeyBinding::new("cmd-j", NextAttention, None),
        KeyBinding::new("space w j", NextAttention, Some(NAV)),
        KeyBinding::new("space s n", NewSection, Some(NAV)),
        KeyBinding::new("space s m", MoveToSection, Some(NAV)),
        KeyBinding::new("space s r", RenameSection, Some(NAV)),
        KeyBinding::new("space s i", SectionIcon, Some(NAV)),
        KeyBinding::new("space s d", DeleteSection, Some(NAV)),
        KeyBinding::new("h", CollapseDir, Some("Sidebar")),
        KeyBinding::new("l", ExpandDir, Some("Sidebar")),
        KeyBinding::new("m", MoveToSection, Some("Sidebar")),
        // Inline section name in the sidebar: the Input lets escape through.
        KeyBinding::new("escape", CancelEdit, Some("SectionName")),
        // New workspace dialog: tab hops between branch and repo filter, space ticks the
        // highlighted repo (names have no spaces), cmd-enter creates from either field.
        KeyBinding::new("tab", SwitchField, Some("NewWorkspace")),
        KeyBinding::new("shift-tab", SwitchField, Some("NewWorkspace")),
        KeyBinding::new("space", ToggleRepo, Some("NewWorkspace > Command")),
        KeyBinding::new("cmd-enter", SubmitForm, Some("NewWorkspace")),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("space ,", OpenSettings, Some(NAV)),
        KeyBinding::new("ctrl-tab", SwitchField, Some("Settings")),
        KeyBinding::new("ctrl-shift-tab", SwitchField, Some("Settings")),
        KeyBinding::new("cmd-/", PickModel, None),
        KeyBinding::new("space m m", PickModel, Some(NAV)),
        KeyBinding::new("] c", NextHunk, Some("DiffPane")),
        KeyBinding::new("[ c", PrevHunk, Some("DiffPane")),
        KeyBinding::new("}", NextFile, Some("DiffPane")),
        KeyBinding::new("{", PrevFile, Some("DiffPane")),
        KeyBinding::new("s", ToggleDiffMode, Some("DiffPane")),
        KeyBinding::new("r", RefreshDiff, Some("DiffPane")),
        KeyBinding::new("space f f", OpenAnyFile, Some(NAV)),
        KeyBinding::new("cmd-p", OpenAnyFile, None),
        KeyBinding::new("c", AddComment, Some("DiffPane")),
        // Plan mode; in the composer shift-tab is caught before the input outdents.
        KeyBinding::new(
            "shift-tab",
            TogglePlanMode,
            Some("Transcript || Queue || PlanPane"),
        ),
        KeyBinding::new("space l l", TogglePlanMode, Some(NAV)),
        KeyBinding::new("space l v", TogglePlan, Some(NAV)),
        KeyBinding::new("space l a", ApprovePlan, Some(NAV)),
        KeyBinding::new("j", SelectNext, Some("PlanPane")),
        KeyBinding::new("k", SelectPrev, Some("PlanPane")),
        KeyBinding::new("g g", SelectFirst, Some("PlanPane")),
        KeyBinding::new("shift-g", SelectLast, Some("PlanPane")),
        KeyBinding::new("ctrl-d", PageDown, Some("PlanPane")),
        KeyBinding::new("ctrl-u", PageUp, Some("PlanPane")),
        KeyBinding::new("] c", NextHunk, Some("PlanPane")),
        KeyBinding::new("[ c", PrevHunk, Some("PlanPane")),
        KeyBinding::new("h", PlanOlder, Some("PlanPane")),
        KeyBinding::new("l", PlanNewer, Some("PlanPane")),
        KeyBinding::new("d", TogglePlanChanges, Some("PlanPane")),
        KeyBinding::new("a", ApprovePlan, Some("PlanPane")),
        KeyBinding::new("i", FocusComposer, Some("PlanPane")),
        KeyBinding::new("shift-c", SendComments, Some("DiffPane")),
    ]);
    // Direct picks, numbered like the model menu: ctrl-cmd-1 / space m 1 is the first row.
    cx.bind_keys((0..chat::MODELS.len()).flat_map(|i| {
        [
            KeyBinding::new(&format!("ctrl-cmd-{}", i + 1), SetModel(i), None),
            KeyBinding::new(&format!("space m {}", i + 1), SetModel(i), Some(NAV)),
        ]
    }));
    cx.bind_keys(
        (0..9).map(|i| KeyBinding::new(&format!("cmd-{}", i + 1), OpenWorkspace(i), None)),
    );
    // The Diff component's scroll actions are private; bind vim keys to them by name.
    let diff = Rc::new(KeyBindingContextPredicate::parse("Diff").unwrap());
    let vim = [("h", "diff::ScrollLeft"), ("l", "diff::ScrollRight")];
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

/// The theme itself is applied by `Shark::new` from the saved appearance (`settings::apply`).
fn install_fonts(cx: &mut App) {
    let fonts = FONTS.iter().map(|f| Cow::Borrowed(*f)).collect();
    cx.text_system().add_fonts(fonts).expect("embedded fonts");
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
            install_fonts(cx);
            bind_keys(cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1280.), px(820.)), cx)),
                titlebar: Some(TitleBar::title_bar_options()),
                ..Default::default()
            };
            open_window(options, cx, |window, cx| {
                cx.new(|cx| app::Shark::new(window, cx))
            })
            .unwrap();
            cx.activate(true);
        });
}
