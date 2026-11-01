//! The cmd-k palette and the cmd-k style menus built on it.
use super::*;
use crate::apps::{self, Kind, OpenApp};
use crate::ui::app_icon;

/// The cmd-k bar look: a bare 640px dialog near the top, on the menus' `popover` surface with
/// their 4px inset, holding a borderless `Command`.
pub(super) fn menu_dialog(d: Dialog, cx: &App) -> Dialog {
    crate::ui::dialog(d)
        .w(px(640.))
        .p(px(4.))
        .bg(cx.theme().popover)
        .margin_top(px(96.))
        .close_button(false)
}

/// Opens a cmd-k style menu over `target`; `on_pick` gets the confirmed row.
pub(crate) fn open_menu<T: 'static>(
    target: WeakEntity<T>,
    placeholder: &'static str,
    items: Vec<CommandItem>,
    selected: usize,
    on_pick: impl Fn(&mut T, usize, &mut Window, &mut Context<T>) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let groups = vec![(None, items)];
    open_grouped(target, placeholder, groups, selected, on_pick, window, cx)
}

/// [`open_menu`] with headed groups; `selected` and the picked row count down through all
/// the groups, as if they were one list.
pub(crate) fn open_grouped<T: 'static>(
    target: WeakEntity<T>,
    placeholder: &'static str,
    groups: Vec<(Option<&'static str>, Vec<CommandItem>)>,
    selected: usize,
    on_pick: impl Fn(&mut T, usize, &mut Window, &mut Context<T>) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let starts: Rc<Vec<usize>> = Rc::new(
        (groups.iter())
            .scan(0, |n, (_, items)| {
                *n += items.len();
                Some(*n - items.len())
            })
            .collect(),
    );
    let command = move |state: &Entity<CommandState>| {
        groups.iter().fold(
            Command::new(state).bordered(false).placeholder(placeholder),
            |c, (heading, items)| {
                let g = CommandGroup::new().items(items.clone());
                c.group(match heading {
                    Some(h) => g.label(*h),
                    None => g,
                })
            },
        )
    };
    let palette = cx.new(|cx| CommandState::new(window, cx));
    // The menu only learns its items when it renders; install them now so the
    // highlight can start on `selected`. The dialog's renders keep it.
    RenderOnce::render(command(&palette), window, cx);
    let section = starts.iter().rposition(|&s| s <= selected).unwrap_or(0);
    let at = IndexPath::new(selected - starts[section]).section(section);
    palette.update(cx, |p, cx| p.set_selected_index(Some(at), window, cx));
    let on_pick = Rc::new(on_pick);
    let state = palette.clone();
    window.open_dialog(cx, move |d, _, cx| {
        let (target, on_pick, starts) = (target.clone(), on_pick.clone(), starts.clone());
        menu_dialog(d, cx).child(command(&state).on_confirm(move |ix, window, cx| {
            window.close_dialog(cx);
            let row = starts[ix.section] + ix.row;
            target.update(cx, |s, cx| on_pick(s, row, window, cx)).ok();
        }))
    });
    palette.update(cx, |p, cx| p.focus(window, cx));
}

type Cmd = fn(&mut Shark, &mut Window, &mut Context<Shark>);

/// A palette row: label, icon, the action whose shortcut it shows, extra search words, and
/// what it does. Without a handler the row dispatches its action, as the key would.
pub(super) struct Row(
    pub &'static str,
    pub IconName,
    pub Option<&'static dyn Action>,
    pub &'static str,
    pub Option<Cmd>,
);

impl Row {
    /// A row with no handler of its own: it dispatches `action`.
    const fn key(
        label: &'static str,
        icon: IconName,
        action: &'static dyn Action,
        words: &'static str,
    ) -> Row {
        Row(label, icon, Some(action), words, None)
    }

    /// Runs the row's handler, else dispatches its action.
    pub(super) fn run(&self, s: &mut Shark, window: &mut Window, cx: &mut Context<Shark>) {
        match (self.4, self.2) {
            (Some(f), _) => f(s, window, cx),
            (None, Some(a)) => window.dispatch_action(a.boxed_clone(), cx),
            _ => {}
        }
    }
}

/// Grouped the way people reach for them; `GROUPS` names each group by its first row.
pub(super) const COMMANDS: &[Row] = &[
    Row::key(
        "New workspace…",
        IconName::Plus,
        &NewWorkspace,
        "create branch",
    ),
    Row::key(
        "Open workspace…",
        IconName::Search,
        &OpenWorkspacePicker,
        "switch recent repository",
    ),
    Row::key(
        "Open next workspace needing attention",
        IconName::CircleAlert,
        &NextAttention,
        "jump waiting",
    ),
    Row::key("Add repo…", IconName::FolderPlus, &AddRepo, "folder git"),
    Row::key(
        "Add repo to workspace…",
        IconName::FolderPlus,
        &AddRepoToWorkspace,
        "",
    ),
    Row::key(
        "Run or stop run scripts",
        IconName::Play,
        &RunScripts,
        "dev server start scripts repo",
    ),
    Row(
        "Show script output",
        IconName::ScrollText,
        None,
        "log run setup archive scripts",
        Some(|s, w, cx| s.show_script_log(w, cx)),
    ),
    Row::key(
        "Archive workspace",
        IconName::Archive,
        &ArchiveWorkspace,
        "remove close",
    ),
    Row::key(
        "Force archive workspace…",
        IconName::ArchiveX,
        &ForceArchiveWorkspace,
        "discard changes delete",
    ),
    Row::key(
        "Reveal in Finder",
        IconName::FolderOpen,
        &RevealInFinder,
        "workspace folder",
    ),
    Row::key("Copy workspace path", IconName::Copy, &CopyPath, "folder"),
    Row::key("Copy branch name", IconName::GitBranch, &CopyBranch, ""),
    Row::key("Show chat", IconName::MessageSquare, &ShowChat, "tab"),
    Row::key("New chat", IconName::Plus, &NewChat, "tab"),
    Row::key("Close chat", IconName::X, &CloseChat, "tab"),
    Row(
        "Reopen closed chat…",
        IconName::ArchiveRestore,
        Some(&ReopenChat),
        "restore tab",
        Some(|s, w, cx| s.pick_closed_chat(w, cx)),
    ),
    Row::key("Next chat", IconName::ChevronRight, &NextChat, "tab"),
    Row::key("Previous chat", IconName::ChevronLeft, &PrevChat, "tab"),
    Row::key("Close tab", IconName::X, &CloseTab, "chat diff file"),
    Row::key(
        "Next tab",
        IconName::ChevronRight,
        &NextTab,
        "chat diff file",
    ),
    Row::key(
        "Previous tab",
        IconName::ChevronLeft,
        &PrevTab,
        "chat diff file",
    ),
    Row::key(
        "Ask a side question",
        IconName::MessageCircleQuestionMark,
        &OpenSide,
        "btw aside",
    ),
    Row::key(
        "Change model…",
        IconName::Sparkles,
        &PickModel,
        "opus sonnet haiku",
    ),
    Row::key(
        "Toggle fast mode",
        IconName::Zap,
        &ToggleFastMode,
        "codex speed priority tier",
    ),
    Row::key(
        "Raise thinking level",
        IconName::ChevronUp,
        &RaiseEffort,
        "effort reasoning more",
    ),
    Row::key(
        "Lower thinking level",
        IconName::ChevronDown,
        &LowerEffort,
        "effort reasoning less",
    ),
    Row(
        "Thinking: Low",
        IconName::Brain,
        None,
        "effort reasoning level low",
        Some(|s, w, cx| s.set_effort("low", w, cx)),
    ),
    Row(
        "Thinking: Medium",
        IconName::Brain,
        None,
        "effort reasoning level medium",
        Some(|s, w, cx| s.set_effort("medium", w, cx)),
    ),
    Row(
        "Thinking: High",
        IconName::Brain,
        None,
        "effort reasoning level high",
        Some(|s, w, cx| s.set_effort("high", w, cx)),
    ),
    Row(
        "Thinking: Extra high",
        IconName::Brain,
        None,
        "effort reasoning level xhigh",
        Some(|s, w, cx| s.set_effort("xhigh", w, cx)),
    ),
    Row(
        "Thinking: Max",
        IconName::Brain,
        None,
        "effort reasoning level max",
        Some(|s, w, cx| s.set_effort("max", w, cx)),
    ),
    Row(
        "Thinking: Ultra",
        IconName::Brain,
        None,
        "effort reasoning level ultra",
        Some(|s, w, cx| s.set_effort("ultra", w, cx)),
    ),
    Row::key(
        "Interrupt agent",
        IconName::Square,
        &StopAgent,
        "stop cancel",
    ),
    Row(
        "Start a new conversation",
        IconName::MessageSquarePlus,
        None,
        "forget session thread reset claude codex",
        Some(|s, w, cx| {
            if let Some(chat) = s.chat_or_err(w, cx) {
                if s.home_shown() {
                    s.show(Tab::Chat, w, cx);
                }
                chat.update(cx, |c, cx| c.new_conversation(cx))
            }
        }),
    ),
    Row::key(
        "Manage queued messages",
        IconName::ListOrdered,
        &FocusQueue,
        "queue",
    ),
    Row::key(
        "Toggle plan mode",
        IconName::ClipboardList,
        &TogglePlanMode,
        "",
    ),
    Row::key(
        "Toggle plan pane",
        IconName::ClipboardList,
        &TogglePlan,
        "pane",
    ),
    Row(
        "Copy plan",
        IconName::Copy,
        Some(&CopySelected),
        "clipboard",
        Some(|s, w, cx| s.with_plan(w, cx, |c, w, cx| c.plan.update(cx, |p, cx| p.copy(w, cx)))),
    ),
    Row(
        "Copy selected message",
        IconName::Copy,
        Some(&CopySelected),
        "clipboard reply tool output",
        Some(|s, w, cx| s.with_plan(w, cx, |c, w, cx| c.copy_selected(w, cx))),
    ),
    Row::key("Approve plan and build", IconName::Hammer, &ApprovePlan, ""),
    Row::key(
        "Build plan in a new chat",
        IconName::Hammer,
        &BuildPlanInNewChat,
        "approve",
    ),
    Row::key(
        "Open file…",
        IconName::FileSearch,
        &OpenAnyFile,
        "find go to",
    ),
    Row::key(
        "Show diff",
        IconName::FileDiff,
        &ShowDiff,
        "changes review tab",
    ),
    Row::key(
        "Show Git",
        IconName::GitPullRequest,
        &ShowGit,
        "pull request description comments review resolve tab",
    ),
    Row(
        "Open file in editor",
        IconName::FilePen,
        Some(&OpenFile),
        "",
        Some(|s, w, cx| s.open_cursor_file(w, cx)),
    ),
    Row(
        "Toggle file viewed",
        IconName::Eye,
        Some(&ToggleViewed),
        "mark unviewed",
        Some(|s, w, cx| s.toggle_cursor_viewed(w, cx)),
    ),
    Row(
        "Comment on diff line",
        IconName::MessageSquarePlus,
        Some(&AddComment),
        "review",
        Some(|s, w, cx| s.with_differ(w, cx, Differ::comment_dialog)),
    ),
    Row(
        "Send review…",
        IconName::Send,
        Some(&SendComments),
        "review diff comments agent",
        Some(|s, w, cx| {
            s.show(Tab::Feature(Feature::Diff), w, cx);
            s.with_differ(w, cx, Differ::open_sheet)
        }),
    ),
    Row(
        "Resolve addressed",
        IconName::Check,
        Some(&ResolveAddressed),
        "review comments threads",
        Some(|s, _, cx| s.differ.update(cx, |d, cx| d.resolve_addressed(cx))),
    ),
    Row(
        "Re-queue outdated",
        IconName::RotateCcw,
        Some(&RequeueOutdated),
        "review comments threads pending",
        Some(|s, _, cx| s.differ.update(cx, |d, cx| d.requeue_outdated(cx))),
    ),
    Row(
        "Save file",
        IconName::Save,
        Some(&SaveFile),
        "",
        Some(|s, w, cx| s.save(w, cx)),
    ),
    Row(
        "Edit working copy",
        IconName::Pencil,
        Some(&EditWorkingCopy),
        "diff inline edit in place",
        Some(|s, w, cx| s.edit_working_copy(None, w, cx)),
    ),
    Row::key("Close file", IconName::X, &CloseFile, "editor"),
    Row(
        "Play or pause video",
        IconName::Play,
        Some(&PlayMedia),
        "media movie diff",
        Some(|s, w, cx| s.with_differ(w, cx, |d, w, cx| d.play_media(w, cx))),
    ),
    Row::key(
        "Run next git step",
        IconName::GitPullRequestArrow,
        &GitNext,
        "git commit push",
    ),
    Row::key(
        "Branch menu…",
        IconName::Layers,
        &BranchMenu,
        "stack base parent copy",
    ),
    Row::key("Git menu…", IconName::GitBranch, &GitMenu, ""),
    Row::key("Fetch now", IconName::RefreshCw, &GitFetch, "git sync"),
    Row(
        "Commit and push",
        IconName::GitCommitHorizontal,
        None,
        "git agent queue",
        Some(|s, w, cx| s.with_sync(w, cx, GitSync::commit_and_push)),
    ),
    Row(
        "Pull",
        IconName::ArrowDown,
        None,
        "git fast-forward",
        Some(|s, w, cx| {
            s.with_sync(w, cx, |g, w, cx| {
                g.run_kind(sync::Kind::Pull, "Pull", w, cx)
            })
        }),
    ),
    Row(
        "Push",
        IconName::ArrowUp,
        None,
        "git",
        Some(|s, w, cx| {
            s.with_sync(w, cx, |g, w, cx| {
                g.run_kind(sync::Kind::Push, "Push", w, cx)
            })
        }),
    ),
    Row(
        "Update from base",
        IconName::GitCompareArrows,
        None,
        "git rebase merge main agent",
        Some(|s, w, cx| s.with_sync(w, cx, GitSync::update_from_base)),
    ),
    Row(
        "Change base…",
        IconName::Target,
        None,
        "git rebase retarget main target onto",
        Some(|s, w, cx| s.with_target(w, cx, |s, ws, w, cx| s.change_base(ws.name, w, cx))),
    ),
    Row(
        "New workspace on top…",
        IconName::Layers,
        None,
        "stack stacked branch from child pr",
        Some(|s, w, cx| s.with_target(w, cx, |s, ws, w, cx| s.stack_from(ws.name, false, w, cx))),
    ),
    Row(
        "Insert workspace below…",
        IconName::ListPlus,
        None,
        "stack stacked between parent in-between pr",
        Some(|s, w, cx| s.with_target(w, cx, |s, ws, w, cx| s.stack_from(ws.name, true, w, cx))),
    ),
    Row(
        "Rebase onto parent",
        IconName::GitCompareArrows,
        None,
        "stack restack parent agent",
        Some(|s, w, cx| s.with_sync(w, cx, GitSync::restack_onto_parent)),
    ),
    Row(
        "Go to stack…",
        IconName::Layers,
        None,
        "stack parent child pr navigate",
        Some(|s, w, cx| s.with_target(w, cx, |s, ws, w, cx| s.go_to_stack(ws.name, w, cx))),
    ),
    Row::key(
        "Create or update PRs",
        IconName::GitPullRequestCreate,
        &CreatePrs,
        "pull request github linked",
    ),
    Row::key(
        "Create draft PRs",
        IconName::GitPullRequestDraft,
        &CreateDraftPrs,
        "pull request github",
    ),
    Row(
        "Merge PRs…",
        IconName::GitMerge,
        None,
        "pull request github",
        Some(|s, w, cx| {
            s.with_sync(w, cx, |g, w, cx| {
                g.run_kind(sync::Kind::Merge, "Merge PRs", w, cx)
            })
        }),
    ),
    Row::key(
        "Open pull requests",
        IconName::ExternalLink,
        &OpenPrs,
        "pull request github",
    ),
    Row::key(
        "Move repo earlier in merge order…",
        IconName::ArrowUp,
        &MergeEarlier,
        "",
    ),
    Row::key(
        "Move repo later in merge order…",
        IconName::ArrowDown,
        &MergeLater,
        "",
    ),
    Row(
        "Toggle remote branch deletion after merge",
        IconName::GitBranch,
        None,
        "setting",
        Some(|s, _, cx| s.set_delete_remote(!s.state.delete_remote_branches, cx)),
    ),
    Row::key("New section", IconName::FolderPlus, &NewSection, "group"),
    Row::key(
        "Move workspace to section…",
        IconName::FolderInput,
        &MoveToSection,
        "group",
    ),
    Row::key("Rename section", IconName::Pencil, &RenameSection, "group"),
    Row::key(
        "Change section icon…",
        IconName::Shapes,
        &SectionIcon,
        "group",
    ),
    Row(
        "Toggle section folded",
        IconName::ChevronsUpDown,
        None,
        "fold group",
        Some(|s, w, cx| s.with_section(w, cx, |s, i, _, cx| s.toggle_section(i, cx))),
    ),
    Row::key(
        "Delete section",
        IconName::Trash,
        &DeleteSection,
        "group remove workspaces stay",
    ),
    Row(
        "Go to Home",
        IconName::House,
        Some(&ToggleHome),
        "start overview dashboard",
        Some(|s, w, cx| s.enter_home(w, cx)),
    ),
    Row(
        "Hide Get started",
        IconName::CircleCheck,
        None,
        "checklist onboarding dismiss home",
        Some(|s, _, cx| s.dismiss_checklist(cx)),
    ),
    Row::key(
        "Toggle sidebar",
        IconName::PanelLeft,
        &ToggleSidebar,
        "rail",
    ),
    Row::key(
        "Toggle Zen mode",
        IconName::Fullscreen,
        &ToggleZen,
        "focus deep work distraction free hide chat only",
    ),
    Row::key(
        "Show or hide diff files",
        IconName::ListTree,
        &ToggleFileTree,
        "files tree differ",
    ),
    Row::key(
        "Editor text bigger",
        IconName::Plus,
        &EditorFontBigger,
        "font size zoom in larger",
    ),
    Row::key(
        "Editor text smaller",
        IconName::Minus,
        &EditorFontSmaller,
        "font size zoom out",
    ),
    Row::key(
        "Reset editor text size",
        IconName::RefreshCw,
        &EditorFontReset,
        "font size zoom default",
    ),
    Row::key(
        "Toggle Vim mode",
        IconName::Keyboard,
        &ToggleVim,
        "keys modal",
    ),
    Row::key(
        "Settings",
        IconName::Settings,
        &OpenSettings,
        "preferences theme font prompts",
    ),
    Row::key(
        "Show Pokémon",
        IconName::Sparkles,
        &ShowPokemon,
        "pokemon companion pokedex egg bag shop tokens",
    ),
    Row::key(
        "Show usage",
        IconName::ChartBar,
        &ShowUsage,
        "account allowance limits claude codex",
    ),
    Row::key(
        "Format document",
        IconName::File,
        &FormatDocument,
        "language format lsp",
    ),
    Row::key(
        "Rename symbol…",
        IconName::File,
        &RenameSymbol,
        "language refactor lsp",
    ),
    Row(
        "Go to definition",
        IconName::ArrowRight,
        Some(&gpui_kit::base::input::GoToDefinition),
        "language declaration jump lsp",
        Some(|s, w, cx| s.go_to_definition(w, cx)),
    ),
    Row::key(
        "Find references",
        IconName::Search,
        &FindReferences,
        "language usages lsp",
    ),
    Row::key(
        "Go to symbol…",
        IconName::Search,
        &DocumentSymbols,
        "language document symbols lsp",
    ),
    Row::key(
        "Show signature help",
        IconName::Info,
        &SignatureHelp,
        "language parameters lsp",
    ),
    Row::key(
        "Go back",
        IconName::ArrowLeft,
        &NavigationBack,
        "language navigation previous",
    ),
    Row::key(
        "Go forward",
        IconName::ArrowRight,
        &NavigationForward,
        "language navigation next",
    ),
    Row(
        "Restart language tools",
        IconName::RefreshCw,
        Some(&RestartLanguage),
        "language setup retry lsp",
        Some(|s, w, cx| s.choose_or_restart_language(w, cx)),
    ),
    Row(
        "About Shark",
        IconName::Info,
        None,
        "version",
        Some(|_, w, cx| crate::settings::about(w, cx)),
    ),
];

/// Rows that act on a workspace; left out while there's none to act on.
const NEED_WORKSPACE: &[&str] = &[
    "Show Git",
    "Add repo to workspace…",
    "Run or stop run scripts",
    "Show script output",
    "Archive workspace",
    "Force archive workspace…",
    "Reveal in Finder",
];

/// Rows that only a Claude chat can do; a Codex chat's palette leaves them out.
const CLAUDE_ONLY: &[&str] = &["Ask a side question"];
const CODEX_ONLY: &[&str] = &["Toggle fast mode", "Thinking: Ultra"];

/// The palette's headings, each with the label of the row it starts at.
const GROUPS: &[(&str, &str)] = &[
    ("Workspace", "New workspace…"),
    ("Chat", "Show chat"),
    ("Plan", "Toggle plan mode"),
    ("Files and diff", "Open file…"),
    ("Git", "Run next git step"),
    ("Sections", "New section"),
    ("View", "Go to Home"),
    ("Shark", "Settings"),
];

/// The shortcut to show for `action` from where the palette was opened: the newest one-key
/// binding (palette rows can't show chords), skipping the Vim-only keys.
fn shortcut(action: &dyn Action, window: &Window) -> Option<Keystroke> {
    window
        .bindings_for_action(action)
        .into_iter()
        .rev()
        .filter(|b| {
            !b.predicate()
                .is_some_and(|p| p.to_string().starts_with("Vim >"))
        })
        .find_map(|b| match b.keystrokes() {
            [k] => Some(k.as_keystroke().clone()),
            _ => None,
        })
}

/// Each command with a shortcut from where `window` is focused, as (label, keystroke) like
/// ("New workspace…", "cmd-n"): the live keymap for Home and Ask Shark.
pub(crate) fn keymap_summary(window: &Window) -> Vec<(String, String)> {
    (COMMANDS.iter())
        .filter_map(|Row(label, _, action, ..)| {
            Some((label.to_string(), shortcut((*action)?, window)?.unparse()))
        })
        .collect()
}

/// A palette row: icon and label, with the shortcut right-aligned in the muted colour.
fn row_item(label: impl Into<SharedString>, icon: IconName, key: Option<Keystroke>) -> CommandItem {
    let label = label.into();
    let item = CommandItem::new().label(label.clone()).icon(icon);
    match key {
        None => item,
        Some(key) => item.child(move |_, cx| {
            let muted = cx.theme().muted_foreground;
            h_flex()
                .flex_1()
                .gap_2()
                .items_center()
                .child(Icon::new(icon).size_4().text_color(muted))
                .child(div().flex_1().min_w_0().truncate().child(label.clone()))
                .child(
                    Kbd::new(key.clone())
                        .appearance(false)
                        .text_xs()
                        .text_color(muted),
                )
        }),
    }
}

/// A palette row: a command, or opening the workspace in an app.
enum Pick {
    Cmd(usize),
    App(&'static OpenApp),
}

/// "Open in <app>" with the app's own icon.
fn app_item(app: &'static OpenApp) -> CommandItem {
    let label: SharedString = format!("Open in {}", app.name).into();
    match app.icon.clone() {
        None => row_item(label, app_icon(app), None),
        Some(icon) => image_item(label, icon),
    }
}

/// The palette and app switcher offer the same eligible apps.
pub(super) fn available_apps(target: Option<&Workspace>) -> Vec<&'static OpenApp> {
    let Some(ws) = target else { return vec![] };
    apps::installed()
        .iter()
        .filter(|a| a.kind != Kind::Xcode || !a.targets(None, &ws.repo_paths()).is_empty())
        .collect()
}

impl Shark {
    pub(super) fn launch_app(&mut self, app: &'static OpenApp, path: &Path) {
        app.open(path);
        if self.state.recent_apps.first() != Some(&app.path) {
            self.state.recent_apps.retain(|p| p != &app.path);
            self.state.recent_apps.insert(0, app.path.clone());
            let _ = self.state.save();
        }
    }

    /// Terminals and editors open the target workspace's folder; git clients a repo and
    /// Xcode a project, asking which when there are several.
    pub(super) fn open_in(
        &mut self,
        app: &'static OpenApp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.with_target(window, cx, |s, ws, window, cx| {
            s.open_in_workspace(app, &ws, window, cx)
        });
    }

    pub(super) fn open_in_workspace(
        &mut self,
        app: &'static OpenApp,
        ws: &Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let targets = match app.kind {
            Kind::Terminal | Kind::Folder => vec![(String::new(), ws.dir())],
            Kind::Git | Kind::Xcode => app.targets(None, &ws.repo_paths()),
        };
        let (icon, ask, none) = match app.kind {
            Kind::Xcode => (
                IconName::FileCode,
                "Open which project?",
                "No Xcode project",
            ),
            _ => (IconName::FolderGit2, "Open which repo?", "No repo"),
        };
        match &targets[..] {
            [] => notify_err(window, cx, format!("{none} in {}", ws.name)),
            [(_, path)] => self.launch_app(app, path),
            _ => {
                let items = (targets.iter())
                    .map(|(name, _)| CommandItem::new().label(name.clone()).icon(icon))
                    .collect();
                let pick =
                    move |s: &mut Shark, row: usize, _: &mut Window, _: &mut Context<Shark>| {
                        s.launch_app(app, &targets[row].1)
                    };
                open_menu(cx.entity().downgrade(), ask, items, 0, pick, window, cx)
            }
        }
    }

    pub(super) fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.mark(|s| &mut s.palette_used);
        // Sidebar order with folded sections opened; cmd-n counts the shown rows only.
        let order = self.ws_order(true);
        let shown = self.ws_order(false);
        let codex = self.active_chat().is_some_and(|c| c.read(cx).codex());
        let mut groups: Vec<(Option<&'static str>, Vec<CommandItem>)> = vec![];
        let mut rows = vec![];
        // Xcode only where there's a project; the rest open the folder or ask which repo.
        let target = self.target(window);
        let no_target = target.is_none();
        let mut apps = available_apps(target.as_ref());
        for (ix, Row(label, icon, action, words, _)) in COMMANDS.iter().enumerate() {
            if let Some((heading, _)) = GROUPS.iter().find(|(_, first)| first == label) {
                groups.push((Some(heading), vec![]));
            }
            if (if codex { CLAUDE_ONLY } else { CODEX_ONLY }).contains(label)
                || (no_target && NEED_WORKSPACE.contains(label))
                || (*label == "Show or hide diff files"
                    && (self.tab != Tab::Feature(Feature::Diff) || self.home_shown()))
            {
                continue;
            }
            rows.push(Pick::Cmd(ix));
            let key = action.and_then(|a| shortcut(a, window));
            let item = row_item(*label, *icon, key).keywords(words.split_whitespace());
            let group = &mut groups.last_mut().expect("COMMANDS starts a group").1;
            group.push(item);
            if *label == "Reveal in Finder" {
                for app in apps.drain(..) {
                    rows.push(Pick::App(app));
                    group.push(app_item(app).keywords(["app", "open", "workspace"]));
                }
            }
        }
        let workspaces = order.iter().map(|&ix| {
            let key = (shown.iter().position(|&s| s == ix))
                .filter(|&n| n < 9)
                .and_then(|n| shortcut(&OpenWorkspace(n), window));
            let name = &self.state.workspaces[ix].name;
            row_item(format!("Open {name}"), IconName::GitBranch, key).keywords([
                "go",
                "switch",
                "workspace",
            ])
        });
        groups.push((Some("Open workspace"), workspaces.collect()));
        open_grouped(
            cx.entity().downgrade(),
            "Type a command or search…",
            groups,
            0,
            // Rows count the shown commands, then the workspaces.
            move |s, row, window, cx| match rows.get(row) {
                Some(&Pick::Cmd(ix)) => COMMANDS[ix].run(s, window, cx),
                Some(&Pick::App(app)) => s.open_in(app, window, cx),
                None => s.open_workspace(order[row - rows.len()], window, cx),
            },
            window,
            cx,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin

    #[test]
    fn every_group_starts_at_a_row() {
        assert_eq!(COMMANDS[0].0, GROUPS[0].1, "the first row starts a group");
        for (heading, first) in GROUPS {
            assert!(
                COMMANDS.iter().any(|r| r.0 == *first),
                "{heading}: no row {first}"
            );
        }
        for label in CLAUDE_ONLY.iter().chain(CODEX_ONLY).chain(NEED_WORKSPACE) {
            assert!(COMMANDS.iter().any(|r| r.0 == *label), "no row {label}");
        }
    }
}
