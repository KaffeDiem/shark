//! The cmd-k palette and the cmd-k style menus built on it.
use super::*;
use crate::apps::{self, Kind, OpenApp};
use crate::ui::app_icon;

/// The cmd-k bar look: a bare 640px dialog near the top, on the menus' `popover` surface with
/// their 4px inset, holding a borderless `Command`.
pub(super) fn menu_dialog(d: Dialog, cx: &App) -> Dialog {
    d.w(px(640.))
        .p(px(4.))
        .bg(cx.theme().popover)
        .font_features(inter_features())
        .margin_top(px(96.))
        .entrance(DialogEntrance::None)
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
/// what it does. The handler runs the row; the action is only read for its key binding.
pub(super) struct Row(
    pub &'static str,
    pub IconName,
    pub Option<&'static dyn Action>,
    pub &'static str,
    pub Cmd,
);

/// Grouped the way people reach for them; `GROUPS` names each group by its first row.
pub(super) const COMMANDS: &[Row] = &[
    Row(
        "New workspace…",
        IconName::Plus,
        Some(&NewWorkspace),
        "create branch",
        |s, w, cx| s.new_workspace(s.state.last_repos.clone(), None, w, cx),
    ),
    Row(
        "Open workspace…",
        IconName::Search,
        Some(&OpenWorkspacePicker),
        "switch recent repository",
        |s, w, cx| s.open_workspace_picker(w, cx),
    ),
    Row(
        "Open next workspace needing attention",
        IconName::CircleAlert,
        Some(&NextAttention),
        "jump waiting",
        |s, w, cx| s.next_attention(w, cx),
    ),
    Row(
        "Add repo…",
        IconName::FolderPlus,
        Some(&AddRepo),
        "folder git",
        |s, w, cx| s.add_repo(w, cx),
    ),
    Row(
        "Add repo to workspace…",
        IconName::FolderPlus,
        Some(&AddRepoToWorkspace),
        "",
        |s, w, cx| s.pick_repo_for_workspace(w, cx),
    ),
    Row(
        "Run or stop run scripts",
        IconName::Play,
        Some(&RunScripts),
        "dev server start scripts repo",
        |s, w, cx| s.toggle_run(w, cx),
    ),
    Row(
        "Show script output",
        IconName::ScrollText,
        None,
        "log run setup archive scripts",
        |s, w, cx| s.show_script_log(w, cx),
    ),
    Row(
        "Archive workspace",
        IconName::Archive,
        Some(&ArchiveWorkspace),
        "remove close",
        |s, w, cx| s.archive(false, w, cx),
    ),
    Row(
        "Force archive workspace…",
        IconName::ArchiveX,
        Some(&ForceArchiveWorkspace),
        "discard changes delete",
        |s, w, cx| s.archive(true, w, cx),
    ),
    Row(
        "Reveal in Finder",
        IconName::FolderOpen,
        Some(&RevealInFinder),
        "workspace folder",
        |s, w, cx| s.with_target(w, cx, |ws, _, cx| cx.reveal_path(&ws.dir())),
    ),
    Row(
        "Copy workspace path",
        IconName::Copy,
        Some(&CopyPath),
        "folder",
        |s, w, cx| {
            s.with_target(w, cx, |ws, w, cx| {
                copy(ws.dir().display().to_string(), w, cx)
            })
        },
    ),
    Row(
        "Copy branch name",
        IconName::GitBranch,
        Some(&CopyBranch),
        "",
        |s, w, cx| s.with_target(w, cx, |ws, w, cx| copy(ws.branch.clone(), w, cx)),
    ),
    Row(
        "Show chat",
        IconName::MessageSquare,
        Some(&ShowChat),
        "tab",
        |s, w, cx| s.show(Tab::Chat, w, cx),
    ),
    Row(
        "New chat",
        IconName::Plus,
        Some(&NewChat),
        "tab",
        |s, w, cx| s.new_chat(w, cx),
    ),
    Row(
        "Close chat",
        IconName::X,
        Some(&CloseChat),
        "tab",
        |s, w, cx| s.close_chat(w, cx),
    ),
    Row(
        "Reopen closed chat…",
        IconName::ArchiveRestore,
        Some(&ReopenChat),
        "restore tab",
        |s, w, cx| s.pick_closed_chat(w, cx),
    ),
    Row(
        "Next chat",
        IconName::ChevronRight,
        Some(&NextChat),
        "tab",
        |s, w, cx| s.cycle_chat(1, w, cx),
    ),
    Row(
        "Previous chat",
        IconName::ChevronLeft,
        Some(&PrevChat),
        "tab",
        |s, w, cx| s.cycle_chat(-1, w, cx),
    ),
    Row(
        "Close tab",
        IconName::X,
        Some(&CloseTab),
        "chat diff file",
        |s, w, cx| s.close_tab(w, cx),
    ),
    Row(
        "Next tab",
        IconName::ChevronRight,
        Some(&NextTab),
        "chat diff file",
        |s, w, cx| s.cycle_tab(1, w, cx),
    ),
    Row(
        "Previous tab",
        IconName::ChevronLeft,
        Some(&PrevTab),
        "chat diff file",
        |s, w, cx| s.cycle_tab(-1, w, cx),
    ),
    Row(
        "Ask a side question",
        IconName::MessageCircleQuestionMark,
        Some(&OpenSide),
        "btw aside",
        |s, w, cx| s.open_side(w, cx),
    ),
    Row(
        "Change model…",
        IconName::Sparkles,
        Some(&PickModel),
        "opus sonnet haiku",
        |s, w, cx| s.pick_model(w, cx),
    ),
    Row(
        "Toggle fast mode",
        IconName::Zap,
        Some(&ToggleFastMode),
        "codex speed priority tier",
        |s, w, cx| s.toggle_fast(w, cx),
    ),
    Row(
        "Raise thinking level",
        IconName::ChevronUp,
        Some(&RaiseEffort),
        "effort reasoning more",
        |s, w, cx| s.step_effort(1, w, cx),
    ),
    Row(
        "Lower thinking level",
        IconName::ChevronDown,
        Some(&LowerEffort),
        "effort reasoning less",
        |s, w, cx| s.step_effort(-1, w, cx),
    ),
    Row(
        "Thinking: Low",
        IconName::Brain,
        None,
        "effort reasoning level low",
        |s, w, cx| s.set_effort("low", w, cx),
    ),
    Row(
        "Thinking: Medium",
        IconName::Brain,
        None,
        "effort reasoning level medium",
        |s, w, cx| s.set_effort("medium", w, cx),
    ),
    Row(
        "Thinking: High",
        IconName::Brain,
        None,
        "effort reasoning level high",
        |s, w, cx| s.set_effort("high", w, cx),
    ),
    Row(
        "Thinking: Extra high",
        IconName::Brain,
        None,
        "effort reasoning level xhigh",
        |s, w, cx| s.set_effort("xhigh", w, cx),
    ),
    Row(
        "Thinking: Max",
        IconName::Brain,
        None,
        "effort reasoning level max",
        |s, w, cx| s.set_effort("max", w, cx),
    ),
    Row(
        "Thinking: Ultra",
        IconName::Brain,
        None,
        "effort reasoning level ultra",
        |s, w, cx| s.set_effort("ultra", w, cx),
    ),
    Row(
        "Interrupt agent",
        IconName::Square,
        Some(&StopAgent),
        "stop cancel",
        |s, _, cx| s.stop(cx),
    ),
    Row(
        "Start a new conversation",
        IconName::MessageSquarePlus,
        None,
        "forget session thread reset claude codex",
        |s, w, cx| match s.active_chat().cloned() {
            Some(chat) => chat.update(cx, |c, cx| c.new_conversation(cx)),
            None => notify_err(w, cx, "Open a workspace first"),
        },
    ),
    Row(
        "Manage queued messages",
        IconName::ListOrdered,
        Some(&FocusQueue),
        "queue",
        |s, w, cx| s.focus_queue(w, cx),
    ),
    Row(
        "Toggle plan mode",
        IconName::ClipboardList,
        Some(&TogglePlanMode),
        "",
        |s, w, cx| s.with_plan(w, cx, |c, _, cx| c.set_plan_mode(!c.meta.plan, cx)),
    ),
    Row(
        "Toggle plan pane",
        IconName::ClipboardList,
        Some(&TogglePlan),
        "pane",
        |s, w, cx| s.with_plan(w, cx, |c, w, cx| c.toggle_plan(w, cx)),
    ),
    Row(
        "Copy plan",
        IconName::Copy,
        Some(&CopySelected),
        "clipboard",
        |s, w, cx| s.with_plan(w, cx, |c, w, cx| c.plan.update(cx, |p, cx| p.copy(w, cx))),
    ),
    Row(
        "Copy selected message",
        IconName::Copy,
        Some(&CopySelected),
        "clipboard reply tool output",
        |s, w, cx| s.with_plan(w, cx, |c, w, cx| c.copy_selected(w, cx)),
    ),
    Row(
        "Approve plan and build",
        IconName::Hammer,
        Some(&ApprovePlan),
        "",
        |s, w, cx| s.with_plan(w, cx, |c, w, cx| c.approve_plan(w, cx)),
    ),
    Row(
        "Build plan in a new chat",
        IconName::Hammer,
        Some(&BuildPlanInNewChat),
        "approve",
        |s, w, cx| s.build_plan_in_new_chat(w, cx),
    ),
    Row(
        "Open file…",
        IconName::FileSearch,
        Some(&OpenAnyFile),
        "find go to",
        |s, w, cx| s.open_any_file(w, cx),
    ),
    Row(
        "Show diff",
        IconName::FileDiff,
        Some(&ShowDiff),
        "changes review tab",
        |s, w, cx| s.show(Tab::Feature(Feature::Diff), w, cx),
    ),
    Row(
        "Show Git",
        IconName::GitPullRequest,
        Some(&ShowGit),
        "pull request description comments review resolve tab",
        |s, w, cx| s.show(Tab::Feature(Feature::Git), w, cx),
    ),
    Row(
        "Open file in editor",
        IconName::FilePen,
        Some(&OpenFile),
        "",
        |s, w, cx| s.open_cursor_file(w, cx),
    ),
    Row(
        "Toggle file viewed",
        IconName::Eye,
        Some(&ToggleViewed),
        "mark unviewed",
        |s, w, cx| s.toggle_cursor_viewed(w, cx),
    ),
    Row(
        "Comment on diff line",
        IconName::MessageSquarePlus,
        Some(&AddComment),
        "review",
        |s, w, cx| s.with_differ(w, cx, Differ::comment_dialog),
    ),
    Row(
        "Send review…",
        IconName::Send,
        Some(&SendComments),
        "review diff comments agent",
        |s, w, cx| {
            s.show(Tab::Feature(Feature::Diff), w, cx);
            s.with_differ(w, cx, Differ::open_sheet)
        },
    ),
    Row(
        "Resolve addressed",
        IconName::Check,
        Some(&ResolveAddressed),
        "review comments threads",
        |s, _, cx| s.differ.update(cx, |d, cx| d.resolve_addressed(cx)),
    ),
    Row(
        "Re-queue outdated",
        IconName::RotateCcw,
        Some(&RequeueOutdated),
        "review comments threads pending",
        |s, _, cx| s.differ.update(cx, |d, cx| d.requeue_outdated(cx)),
    ),
    Row(
        "Save file",
        IconName::Save,
        Some(&SaveFile),
        "",
        |s, w, cx| s.save(w, cx),
    ),
    Row(
        "Edit working copy",
        IconName::Pencil,
        Some(&EditWorkingCopy),
        "diff inline edit in place",
        |s, w, cx| s.edit_working_copy(None, w, cx),
    ),
    Row(
        "Close file",
        IconName::X,
        Some(&CloseFile),
        "editor",
        |s, w, cx| s.close_file(w, cx),
    ),
    Row(
        "Play or pause video",
        IconName::Play,
        Some(&PlayMedia),
        "media movie diff",
        |s, w, cx| s.with_differ(w, cx, |d, w, cx| d.play_media(w, cx)),
    ),
    Row(
        "Run next git step",
        IconName::GitPullRequestArrow,
        Some(&GitNext),
        "git commit push",
        |s, w, cx| s.with_sync(w, cx, GitSync::primary),
    ),
    Row(
        "Branch menu…",
        IconName::Layers,
        Some(&BranchMenu),
        "stack base parent copy",
        |s, w, cx| s.open_branch_menu(w, cx),
    ),
    Row(
        "Git menu…",
        IconName::GitBranch,
        Some(&GitMenu),
        "",
        |s, w, cx| s.with_sync(w, cx, GitSync::open_menu),
    ),
    Row(
        "Fetch now",
        IconName::RefreshCw,
        Some(&GitFetch),
        "git sync",
        |s, w, cx| s.with_sync(w, cx, GitSync::fetch_now),
    ),
    Row(
        "Commit and push",
        IconName::GitCommitHorizontal,
        None,
        "git agent queue",
        |s, w, cx| s.with_sync(w, cx, GitSync::commit_and_push),
    ),
    Row(
        "Pull",
        IconName::ArrowDown,
        None,
        "git fast-forward",
        |s, w, cx| {
            s.with_sync(w, cx, |g, w, cx| {
                g.run_kind(sync::Kind::Pull, "Pull", w, cx)
            })
        },
    ),
    Row("Push", IconName::ArrowUp, None, "git", |s, w, cx| {
        s.with_sync(w, cx, |g, w, cx| {
            g.run_kind(sync::Kind::Push, "Push", w, cx)
        })
    }),
    Row(
        "Update from base",
        IconName::GitCompareArrows,
        None,
        "git rebase merge main agent",
        |s, w, cx| s.with_sync(w, cx, GitSync::update_from_base),
    ),
    Row(
        "Change base…",
        IconName::Target,
        None,
        "git rebase retarget main target onto",
        |s, w, cx| match s.target(w) {
            Some(ws) => s.change_base(ws.name, w, cx),
            None => notify_err(w, cx, "Open a workspace first"),
        },
    ),
    Row(
        "New workspace on top…",
        IconName::Layers,
        None,
        "stack stacked branch from child pr",
        |s, w, cx| match s.target(w) {
            Some(ws) => s.stack_from(ws.name, false, w, cx),
            None => notify_err(w, cx, "Open a workspace first"),
        },
    ),
    Row(
        "Insert workspace below…",
        IconName::ListPlus,
        None,
        "stack stacked between parent in-between pr",
        |s, w, cx| match s.target(w) {
            Some(ws) => s.stack_from(ws.name, true, w, cx),
            None => notify_err(w, cx, "Open a workspace first"),
        },
    ),
    Row(
        "Rebase onto parent",
        IconName::GitCompareArrows,
        None,
        "stack restack parent agent",
        |s, w, cx| s.with_sync(w, cx, GitSync::restack_onto_parent),
    ),
    Row(
        "Go to stack…",
        IconName::Layers,
        None,
        "stack parent child pr navigate",
        |s, w, cx| match s.target(w) {
            Some(ws) => s.go_to_stack(ws.name, w, cx),
            None => notify_err(w, cx, "Open a workspace first"),
        },
    ),
    Row(
        "Create or update PRs",
        IconName::GitPullRequestCreate,
        Some(&CreatePrs),
        "pull request github linked",
        |s, w, cx| s.create_prs(w, cx),
    ),
    Row(
        "Create draft PRs",
        IconName::GitPullRequestDraft,
        Some(&CreateDraftPrs),
        "pull request github",
        |s, w, cx| s.create_draft_prs(w, cx),
    ),
    Row(
        "Merge PRs…",
        IconName::GitMerge,
        None,
        "pull request github",
        |s, w, cx| {
            s.with_sync(w, cx, |g, w, cx| {
                g.run_kind(sync::Kind::Merge, "Merge PRs", w, cx)
            })
        },
    ),
    Row(
        "Open pull requests",
        IconName::ExternalLink,
        Some(&OpenPrs),
        "pull request github",
        |s, w, cx| s.with_sync(w, cx, GitSync::open_prs),
    ),
    Row(
        "Move repo earlier in merge order…",
        IconName::ArrowUp,
        Some(&MergeEarlier),
        "",
        |s, w, cx| s.with_sync(w, cx, |g, w, cx| g.move_in_merge_order(true, w, cx)),
    ),
    Row(
        "Move repo later in merge order…",
        IconName::ArrowDown,
        Some(&MergeLater),
        "",
        |s, w, cx| s.with_sync(w, cx, |g, w, cx| g.move_in_merge_order(false, w, cx)),
    ),
    Row(
        "Toggle remote branch deletion after merge",
        IconName::GitBranch,
        None,
        "setting",
        |s, _, cx| s.set_delete_remote(!s.state.delete_remote_branches, cx),
    ),
    Row(
        "New section",
        IconName::FolderPlus,
        Some(&NewSection),
        "group",
        |s, w, cx| s.start_naming(None, None, w, cx),
    ),
    Row(
        "Move workspace to section…",
        IconName::FolderInput,
        Some(&MoveToSection),
        "group",
        |s, w, cx| s.pick_section(w, cx),
    ),
    Row(
        "Rename section",
        IconName::Pencil,
        Some(&RenameSection),
        "group",
        |s, w, cx| s.with_section(w, cx, |s, i, w, cx| s.start_naming(Some(i), None, w, cx)),
    ),
    Row(
        "Change section icon…",
        IconName::Shapes,
        Some(&SectionIcon),
        "group",
        |s, w, cx| s.with_section(w, cx, Shark::pick_icon),
    ),
    Row(
        "Toggle section folded",
        IconName::ChevronsUpDown,
        None,
        "fold group",
        |s, w, cx| s.with_section(w, cx, |s, i, _, cx| s.toggle_section(i, cx)),
    ),
    Row(
        "Delete section",
        IconName::Trash,
        Some(&DeleteSection),
        "group remove workspaces stay",
        |s, w, cx| s.with_section(w, cx, Shark::delete_section),
    ),
    Row(
        "Go to Home",
        IconName::House,
        Some(&ToggleHome),
        "start overview dashboard",
        |s, w, cx| s.enter_home(w, cx),
    ),
    Row(
        "Hide Get started",
        IconName::CircleCheck,
        None,
        "checklist onboarding dismiss home",
        |s, _, cx| s.dismiss_checklist(cx),
    ),
    Row(
        "Toggle sidebar",
        IconName::PanelLeft,
        Some(&ToggleSidebar),
        "rail",
        |s, _, cx| s.toggle_sidebar(cx),
    ),
    Row(
        "Toggle Zen mode",
        IconName::Fullscreen,
        Some(&ToggleZen),
        "focus deep work distraction free hide chat only",
        |s, w, cx| s.toggle_zen(w, cx),
    ),
    Row(
        "Show or hide diff files",
        IconName::ListTree,
        Some(&ToggleFileTree),
        "files tree differ",
        |s, w, cx| s.toggle_tree(w, cx),
    ),
    Row(
        "Toggle Vim mode",
        IconName::Keyboard,
        Some(&ToggleVim),
        "keys modal",
        |s, w, cx| s.toggle_vim(w, cx),
    ),
    Row(
        "Settings",
        IconName::Settings,
        Some(&OpenSettings),
        "preferences theme font prompts",
        |s, w, cx| crate::settings::open(&s.state, w, cx),
    ),
    Row(
        "Show usage",
        IconName::ChartBar,
        Some(&ShowUsage),
        "account allowance limits claude codex",
        |s, w, cx| s.show_usage(w, cx),
    ),
    Row(
        "About Shark",
        IconName::Info,
        None,
        "version",
        |_, w, cx| crate::settings::about(w, cx),
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
        Some(icon) => CommandItem::new().label(label.clone()).child(move |_, _| {
            h_flex()
                .flex_1()
                .gap_2()
                .items_center()
                .child(img(icon.clone()).size_4().flex_none())
                .child(div().flex_1().min_w_0().truncate().child(label.clone()))
        }),
    }
}

/// A workspace's repos as (name, path).
pub(super) fn repo_paths(ws: &Workspace) -> Vec<(String, PathBuf)> {
    (ws.repos.iter())
        .map(|r| (r.name.clone(), ws.dir().join(&r.name)))
        .collect()
}

/// The palette and app switcher offer the same eligible apps.
pub(super) fn available_apps(target: Option<&Workspace>) -> Vec<&'static OpenApp> {
    let Some(ws) = target else { return vec![] };
    apps::installed()
        .iter()
        .filter(|a| a.kind != Kind::Xcode || !a.targets(None, &repo_paths(ws)).is_empty())
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
        let Some(ws) = self.target(window) else {
            return notify_err(window, cx, "Open a workspace first");
        };
        self.open_in_workspace(app, &ws, window, cx);
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
            Kind::Git | Kind::Xcode => app.targets(None, &repo_paths(ws)),
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
                Some(&Pick::Cmd(ix)) => (COMMANDS[ix].4)(s, window, cx),
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
