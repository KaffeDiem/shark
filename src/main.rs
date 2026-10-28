mod agent;
mod app;
mod apps;
mod attention;
mod chat;
mod checkbox;
mod codex;
#[cfg(test)]
mod codex_live;
mod complete;
mod deps;
mod diff_view;
mod editor;
mod features;
mod file_icon;
mod files;
mod git;
mod lsp;
mod menu;
mod menubar;
mod overflow_fade;
mod plan;
mod pr;
mod prompts;
mod settings;
mod stack;
mod state;
mod sync;
mod ui;
mod usage;
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
        CycleOpenApp,
        OpenWorkspacePicker,
        AddRepoToWorkspace,
        StopAgent,
        FocusLeft,
        FocusRight,
        ShowChat,
        ShowUsage,
        ShowDiff,
        ShowGit,
        CreatePrs,
        OpenPrs,
        CreateDraftPrs,
        MergeEarlier,
        MergeLater,
        GitNext,
        GitMenu,
        BranchMenu,
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
        CopyPath,
        CopyBranch,
        ToggleRepo,
        SwitchField,
        SubmitForm,
        PickBranch,
        PickPullRequest,
        BranchFromSource,
        RefreshWorkspaceSources,
        NextAttention,
        OpenSettings,
        EditQueued,
        DeleteQueued,
        MoveQueuedUp,
        MoveQueuedDown,
        FocusQueue,
        NewChat,
        CloseChat,
        CloseTab,
        ReopenChat,
        NextChat,
        PrevChat,
        NextTab,
        PrevTab,
        AskSide,
        OpenSide,
        CloseSide,
        CopySide,
        CopySelected,
        InsertSide,
        NewSection,
        MoveToSection,
        RenameSection,
        SectionIcon,
        DeleteSection,
        CancelEdit,
        ToggleHome,
        FormatDocument,
        RenameSymbol,
        FindReferences,
        DocumentSymbols,
        SignatureHelp,
        NavigationBack,
        NavigationForward,
        RestartLanguage,
        EditorFontBigger,
        EditorFontSmaller,
        EditorFontReset,
    ]
);

// Plan mode: shift-tab, the plan pane and its keys. Fast mode (codex): cmd-shift-f. Thinking
// level: cmd-shift-. up, cmd-shift-, down.
actions!(
    shark,
    [
        TogglePlanMode,
        ToggleFastMode,
        RaiseEffort,
        LowerEffort,
        TogglePlan,
        ApprovePlan,
        BuildPlanInNewChat,
        PlanOlder,
        PlanNewer,
        TogglePlanChanges
    ]
);

// Editor and diff review: cmd-p, and `c` / `shift-c` in the diff.
actions!(
    shark,
    [
        OpenAnyFile,
        AddComment,
        SendComments,
        PlayMedia,
        EditWorkingCopy
    ]
);

// The review queue (src/features/differ/review.rs): threads at the diff cursor, the composer, the sheet.
actions!(
    shark,
    [
        EditThread,
        DeleteThread,
        ViewChange,
        ResolveThread,
        ReopenThread,
        NextThread,
        PrevThread,
        SuggestChange,
        ToggleRow,
        ResolveAddressed,
        RequeueOutdated
    ]
);

/// Switch the open workspace to `chat::MODELS[n]` without opening the menu.
#[derive(Clone, PartialEq, Debug, serde::Deserialize, Action)]
#[action(namespace = shark, no_json)]
pub struct SetModel(pub usize);

/// Open the nth sidebar workspace (cmd-1 is the top one).
#[derive(Clone, PartialEq, Debug, serde::Deserialize, Action)]
#[action(namespace = shark, no_json)]
pub struct OpenWorkspace(pub usize);

/// Open the nth tab of the current workspace (ctrl-1 is the leftmost one).
#[derive(Clone, PartialEq, Debug, serde::Deserialize, Action)]
#[action(namespace = shark, no_json)]
pub struct OpenTab(pub usize);

fn bind_keys(cx: &mut App) {
    // First, so the bindings below take precedence, as when gpui-component bound them.
    diff_view::init(cx);
    // Vim-style keys sit under `Vim`, which the root view carries while Vim mode is on
    // (Settings › General). The arrows, letters that name an action and cmd shortcuts work
    // either way. VimNormal: the editor in Vim's Normal mode.
    const NAV: &str = "Sidebar || Home || Transcript || DiffPane || FileTree || Queue || PlanPane || GitFeature || VimNormal";
    const LIST: &str = "Sidebar || Home || Transcript || FileTree || Queue";
    // The diff's letter keys; not while typing in the review composer, which sits in the diff.
    const DIFF: &str = "DiffPane && !Input";
    // The diff's line cursor (src/features/differ/review.rs) moves like a list. With Vim its arrows
    // scroll natively and j/k move the cursor; without, the arrows move it.
    const MOVE: &str = "Sidebar || Home || Transcript || FileTree || Queue || DiffPane || PlanPane || GitFeature || Features";
    const ARROWS: &str = "Sidebar || Home || Transcript || FileTree || Queue || PlanPane || Features || (GitFeature && !Input) || (Diff && !Vim)";
    // `!Input` sits outside `Vim > (…)`: inside it, `!` sees only the contexts down to the
    // match, not a focused Input below. The editor's Normal mode is an Input that types nothing.
    let vim = |keys: &str, action: Box<dyn Action>, context: &str| {
        let predicate = format!("(Vim > ({context})) && (!Input || VimNormal)");
        KeyBinding::load(
            keys,
            action,
            Some(Rc::new(
                KeyBindingContextPredicate::parse(&predicate).unwrap(),
            )),
            false,
            None,
            &DummyKeyboardMapper,
        )
        .unwrap()
    };
    cx.bind_keys([
        KeyBinding::new("cmd-k", TogglePalette, None),
        KeyBinding::new("cmd-shift-p", TogglePalette, None),
        KeyBinding::new("cmd-n", NewWorkspace, None),
        KeyBinding::new("cmd-o", CycleOpenApp, None),
        KeyBinding::new("cmd-shift-o", OpenWorkspacePicker, Some("!FileEditor")),
        KeyBinding::new("cmd-0", ToggleHome, None),
        // Home: esc goes back to the workspace (or stops an Ask Shark answer); tab switches
        // the prompt between new workspace and Ask Shark; i/a jump from the list to it.
        KeyBinding::new("escape", CancelEdit, Some("HomeView")),
        KeyBinding::new("tab", SwitchField, Some("HomePrompt")),
        KeyBinding::new("i", FocusComposer, Some("Home")),
        KeyBinding::new("a", FocusComposer, Some("Home")),
        // The workspace under the cursor, as in the row menu.
        KeyBinding::new("cmd-backspace", ArchiveWorkspace, Some("Home || Sidebar")),
        // Not in the editor: Vim users leave insert mode with ctrl-c.
        KeyBinding::new("ctrl-c", StopAgent, Some("!FileEditor")),
        KeyBinding::new("cmd-alt-left", FocusLeft, None),
        KeyBinding::new("cmd-alt-right", FocusRight, None),
        // Anywhere under the root while it carries `Vim`, like their cmd-alt twins.
        KeyBinding::new("ctrl-w h", FocusLeft, Some("Vim")),
        KeyBinding::new("ctrl-w l", FocusRight, Some("Vim")),
        KeyBinding::new("cmd-b", ToggleSidebar, None),
        KeyBinding::new("cmd-alt-b", ToggleFileTree, None),
        KeyBinding::new("cmd-shift-d", ShowDiff, None),
        KeyBinding::new("cmd-shift-g", ShowGit, None),
        KeyBinding::new("ctrl-cmd-v", ToggleVim, None),
        // In the diff: the file last moved to with `}`/`{` or the tree.
        KeyBinding::new("v", ToggleViewed, Some("FileTree || (DiffPane && !Input)")),
        KeyBinding::new("o", OpenFile, Some("FileTree || (DiffPane && !Input)")),
        KeyBinding::new("left", CollapseDir, Some("FileTree || Sidebar")),
        KeyBinding::new("right", ExpandDir, Some("FileTree || Sidebar")),
        KeyBinding::new("cmd-s", SaveFile, Some("FileEditor")),
        KeyBinding::new("alt-shift-f", FormatDocument, Some("FileEditor")),
        KeyBinding::new("f2", RenameSymbol, Some("FileEditor")),
        KeyBinding::new(
            "f12",
            gpui_kit::base::input::GoToDefinition,
            Some("FileEditor"),
        ),
        KeyBinding::new("shift-f12", FindReferences, Some("FileEditor")),
        KeyBinding::new("cmd-shift-o", DocumentSymbols, Some("FileEditor")),
        KeyBinding::new("cmd-shift-space", SignatureHelp, Some("FileEditor")),
        KeyBinding::new("ctrl--", NavigationBack, Some("FileEditor")),
        // macOS reports Control-Shift-minus as `ctrl-_`.
        KeyBinding::new("ctrl-_", NavigationForward, Some("FileEditor")),
        KeyBinding::new("cmd-w", CloseFile, Some("FileEditor")),
        KeyBinding::new("cmd-=", EditorFontBigger, Some("FileEditor || DiffPane")),
        KeyBinding::new("cmd-+", EditorFontBigger, Some("FileEditor || DiffPane")),
        KeyBinding::new("cmd--", EditorFontSmaller, Some("FileEditor || DiffPane")),
        KeyBinding::new("cmd-t", NewChat, None),
        KeyBinding::new("cmd-w", CloseTab, None),
        KeyBinding::new("cmd-shift-t", ReopenChat, None),
        KeyBinding::new("cmd-shift-]", NextChat, None),
        KeyBinding::new("cmd-shift-[", PrevChat, None),
        KeyBinding::new("cmd-}", NextChat, None),
        KeyBinding::new("cmd-{", PrevChat, None),
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PrevTab, None),
        KeyBinding::new("cmd-shift-enter", AskSide, Some("Composer")),
        KeyBinding::new("escape", CloseSide, Some("SideAnswer")),
        KeyBinding::new("c", CopySide, Some("SideAnswer")),
        KeyBinding::new("enter", InsertSide, Some("SideAnswer")),
        // The agent's questions (src/chat/question.rs): the form takes its own keys; esc on
        // its choices closes it so the user answers in the chat.
        KeyBinding::new("escape", CancelEdit, Some("Questionnaire && !Input")),
        KeyBinding::new("down", SelectNext, Some(ARROWS)),
        KeyBinding::new("up", SelectPrev, Some(ARROWS)),
        KeyBinding::new("home", SelectFirst, Some(ARROWS)),
        KeyBinding::new("end", SelectLast, Some(ARROWS)),
        KeyBinding::new("pagedown", PageDown, Some(ARROWS)),
        KeyBinding::new("pageup", PageUp, Some(ARROWS)),
        KeyBinding::new("enter", Confirm, Some(LIST)),
        KeyBinding::new("enter", Confirm, Some("Features")),
        KeyBinding::new("o", Confirm, Some("Transcript")),
        KeyBinding::new("y", CopySelected, Some("Transcript || PlanPane")),
        KeyBinding::new("i", FocusComposer, Some("Transcript")),
        KeyBinding::new("a", FocusComposer, Some("Transcript")),
        KeyBinding::new("escape", FocusTranscript, Some("Composer")),
        KeyBinding::new("e", EditQueued, Some("Queue")),
        KeyBinding::new("d", DeleteQueued, Some("Queue")),
        KeyBinding::new("backspace", DeleteQueued, Some("Queue")),
        KeyBinding::new("alt-up", MoveQueuedUp, Some("Queue")),
        KeyBinding::new("alt-down", MoveQueuedDown, Some("Queue")),
        KeyBinding::new("escape", FocusComposer, Some("Queue")),
        KeyBinding::new("i", FocusComposer, Some("Queue")),
        KeyBinding::new("a", FocusComposer, Some("Queue")),
        KeyBinding::new("cmd-j", NextAttention, None),
        KeyBinding::new("m", MoveToSection, Some("Sidebar")),
        // Inline section name in the sidebar: the Input lets escape through.
        KeyBinding::new("escape", CancelEdit, Some("SectionName")),
        // New workspace dialog: tab hops between branch and repo filter, space ticks the
        // highlighted repo (names have no spaces), cmd-b lists its branches, cmd-enter creates
        // from either field.
        KeyBinding::new("tab", SwitchField, Some("NewWorkspace")),
        KeyBinding::new("shift-tab", SwitchField, Some("NewWorkspace")),
        KeyBinding::new("space", ToggleRepo, Some("NewWorkspace > Command")),
        KeyBinding::new("cmd-enter", SubmitForm, Some("NewWorkspace")),
        KeyBinding::new("cmd-b", PickBranch, Some("NewWorkspace")),
        KeyBinding::new("cmd-p", PickPullRequest, Some("NewWorkspace")),
        KeyBinding::new("alt-enter", BranchFromSource, Some("NewWorkspace")),
        KeyBinding::new("cmd-r", RefreshWorkspaceSources, Some("NewWorkspace")),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("ctrl-tab", SwitchField, Some("Settings")),
        KeyBinding::new("ctrl-shift-tab", SwitchField, Some("Settings")),
        KeyBinding::new("cmd-/", PickModel, None),
        KeyBinding::new("cmd-shift-f", ToggleFastMode, None),
        KeyBinding::new("cmd-shift-.", RaiseEffort, None),
        KeyBinding::new("cmd-shift-,", LowerEffort, None),
        KeyBinding::new("cmd->", RaiseEffort, None),
        KeyBinding::new("cmd-<", LowerEffort, None),
        KeyBinding::new(
            "alt-down",
            NextHunk,
            Some("(DiffPane && !Input) || PlanPane"),
        ),
        KeyBinding::new("alt-up", PrevHunk, Some("(DiffPane && !Input) || PlanPane")),
        KeyBinding::new("}", NextFile, Some(DIFF)),
        KeyBinding::new("{", PrevFile, Some(DIFF)),
        KeyBinding::new("s", ToggleDiffMode, Some(DIFF)),
        KeyBinding::new("r", RefreshDiff, Some(DIFF)),
        KeyBinding::new("p", PlayMedia, Some(DIFF)),
        KeyBinding::new("cmd-p", OpenAnyFile, None),
        // Edit the cursor file's working copy in the diff; cmd-s saves it there.
        KeyBinding::new("i", EditWorkingCopy, Some(DIFF)),
        // Back to reviewing, also when the line input lost focus.
        KeyBinding::new("escape", CancelEdit, Some(DIFF)),
        // Also when the line input lost focus (its row scrolled away).
        KeyBinding::new("cmd-s", SaveFile, Some("DiffPane")),
        KeyBinding::new("cmd-s", SaveFile, Some("GitFeature")),
        KeyBinding::new("escape", CancelEdit, Some("GitFeature")),
        KeyBinding::new("c", AddComment, Some(DIFF)),
        // Plan mode; in the composer shift-tab is caught before the input outdents.
        KeyBinding::new(
            "shift-tab",
            TogglePlanMode,
            Some("Transcript || Queue || PlanPane"),
        ),
        KeyBinding::new("left", PlanOlder, Some("PlanPane")),
        KeyBinding::new("right", PlanNewer, Some("PlanPane")),
        KeyBinding::new("d", TogglePlanChanges, Some("PlanPane")),
        KeyBinding::new("a", ApprovePlan, Some("PlanPane")),
        KeyBinding::new("n", BuildPlanInNewChat, Some("PlanPane")),
        KeyBinding::new("i", FocusComposer, Some("PlanPane")),
        KeyBinding::new("shift-c", SendComments, Some(DIFF)),
        // Threads at the diff cursor. `r` resolves one, else falls through to RefreshDiff.
        KeyBinding::new("e", EditThread, Some(DIFF)),
        KeyBinding::new("x", DeleteThread, Some(DIFF)),
        KeyBinding::new("d", ViewChange, Some(DIFF)),
        KeyBinding::new("r", ResolveThread, Some(DIFF)),
        KeyBinding::new("shift-r", ReopenThread, Some(DIFF)),
        KeyBinding::new("] c", NextThread, Some(DIFF)),
        KeyBinding::new("[ c", PrevThread, Some(DIFF)),
        // The composer takes cmd-enter and escape from its textarea itself.
        KeyBinding::new("cmd-shift-s", SuggestChange, Some("ReviewComposer")),
        KeyBinding::new("space", ToggleRow, Some("ReviewSheet")),
        KeyBinding::new("enter", Confirm, Some("ReviewSheet")),
        KeyBinding::new("escape", CancelEdit, Some("ReviewSheet")),
        KeyBinding::new("cmd-enter", SubmitForm, Some("ReviewSheet")),
        KeyBinding::new("down", SelectNext, Some("ReviewSheet")),
        KeyBinding::new("up", SelectPrev, Some("ReviewSheet")),
        KeyBinding::new("j", SelectNext, Some("ReviewSheet")),
        KeyBinding::new("k", SelectPrev, Some("ReviewSheet")),
    ]);
    cx.bind_keys([
        vim("] c", Box::new(NextHunk), "PlanPane"),
        vim("[ c", Box::new(PrevHunk), "PlanPane"),
        // With threads in the diff, ]c / [c jump between them (the design's keys); without,
        // they still go to the next change. alt-down / alt-up always do.
        vim("] c", Box::new(NextThread), DIFF),
        vim("[ c", Box::new(PrevThread), DIFF),
        vim("shift-k", Box::new(MoveQueuedUp), "Queue"),
        vim("shift-j", Box::new(MoveQueuedDown), "Queue"),
        // The editor's jump list, as in Vim (`g s`, the outline, is a Vim command).
        vim("ctrl-o", Box::new(NavigationBack), "VimNormal"),
        vim("ctrl-i", Box::new(NavigationForward), "VimNormal"),
        vim("g t", Box::new(NextChat), NAV),
        vim("g shift-t", Box::new(PrevChat), NAV),
        vim("j", Box::new(SelectNext), MOVE),
        vim("k", Box::new(SelectPrev), MOVE),
        vim("g g", Box::new(SelectFirst), MOVE),
        vim("shift-g", Box::new(SelectLast), MOVE),
        vim("ctrl-d", Box::new(PageDown), MOVE),
        vim("ctrl-u", Box::new(PageUp), MOVE),
        vim("h", Box::new(CollapseDir), "FileTree || Sidebar"),
        vim("l", Box::new(ExpandDir), "FileTree || Sidebar"),
        vim("h", Box::new(PlanOlder), "PlanPane"),
        vim("l", Box::new(PlanNewer), "PlanPane"),
    ]);
    // Direct picks, numbered like the model menu: ctrl-cmd-1 is the first row.
    cx.bind_keys(
        (0..chat::MODELS.len())
            .map(|i| KeyBinding::new(&format!("ctrl-cmd-{}", i + 1), SetModel(i), None)),
    );
    cx.bind_keys(
        (0..9).map(|i| KeyBinding::new(&format!("cmd-{}", i + 1), OpenWorkspace(i), None)),
    );
    cx.bind_keys((0..9).map(|i| KeyBinding::new(&format!("ctrl-{}", i + 1), OpenTab(i), None)));
    // The Diff component's scroll actions are private; bind vim keys to them by name.
    let diff = Rc::new(KeyBindingContextPredicate::parse("(Vim > Diff) && !Input").unwrap());
    let scroll = [
        ("h", "shark_diff::ScrollLeft"),
        ("l", "shark_diff::ScrollRight"),
    ];
    let bindings: Vec<KeyBinding> = scroll
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
    #[cfg(debug_assertions)]
    let icon = include_bytes!("../assets/icon-dev.png").as_slice();
    #[cfg(not(debug_assertions))]
    let icon = include_bytes!("../assets/icon.png").as_slice();
    let data = NSData::with_bytes(icon);
    let image = NSImage::initWithData(NSImage::alloc(), &data).expect("embedded app icon");
    unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image)) };
}

fn main() {
    if cfg!(test) {
        #[cfg(all(test, feature = "test-support"))]
        app::ui_tests::chrome_tests::render_pixels();
        return;
    }
    // Apps launched from Finder get a minimal PATH; borrow the login shell's so claude/gh/git
    // resolve. The dependency check shows the error.
    match deps::login_path() {
        // SAFETY: first thing in main; the only other thread is cmd_bounded's waiter, which
        // has already handed over the output and never reads the environment.
        Ok(p) => unsafe { std::env::set_var("PATH", p) },
        Err(e) => _ = deps::PATH_ERROR.set(e),
    }
    // LaunchServices and the icons take a moment; the first menu shouldn't wait for them.
    std::thread::spawn(apps::installed);
    application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(|cx| {
            set_dock_icon();
            gpui_kit::init(cx);
            cx.on_app_quit(|_| lsp::client::Client::terminate_all())
                .detach();
            install_fonts(cx);
            bind_keys(cx);
            menubar::init(cx);
            let options = WindowOptions {
                app_owns_titlebar_drag: true,
                window_bounds: Some(WindowBounds::centered(size(px(1280.), px(820.)), cx)),
                titlebar: Some(TitlebarOptions {
                    traffic_light_position: Some(point(px(9.), px(15.))),
                    ..TitleBar::title_bar_options()
                }),
                ..Default::default()
            };
            let (window, shark) = open_window(options, cx, |window, cx| {
                cx.new(|cx| app::Shark::new(window, cx))
            })
            .unwrap();
            // Global, not on Shark's root: an open dialog isn't inside Shark, and quitting
            // from it must still confirm unsaved edits. Deferred, like `menubar::in_window`.
            let shark = shark.downgrade();
            cx.on_action(move |_: &menubar::Quit, cx| {
                let shark = shark.clone();
                cx.defer(move |cx| {
                    let asked =
                        window.update(cx, |_, w, cx| shark.update(cx, |s, cx| s.quit(w, cx)));
                    if !matches!(asked, Ok(Ok(()))) {
                        cx.quit()
                    }
                });
            });
            cx.activate(true);
        });
}
