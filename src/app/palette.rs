//! The cmd-k palette and the cmd-k style menus built on it.
use super::*;

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
    let palette = cx.new(|cx| CommandState::new(window, cx));
    // The menu only learns its items when it renders; install them now so the
    // highlight can start on `selected`. The dialog's renders keep it.
    RenderOnce::render(
        Command::new(&palette)
            .placeholder(placeholder)
            .items(items.clone()),
        window,
        cx,
    );
    palette.update(cx, |p, cx| {
        p.set_selected_index(Some(IndexPath::new(selected)), window, cx)
    });
    let on_pick = Rc::new(on_pick);
    let state = palette.clone();
    window.open_dialog(cx, move |d, _, cx| {
        let (target, on_pick) = (target.clone(), on_pick.clone());
        menu_dialog(d, cx).child(
            Command::new(&state)
                .bordered(false)
                .placeholder(placeholder)
                .items(items.clone())
                .on_confirm(
            move |ix, window, cx| {
                window.close_dialog(cx);
                target
                    .update(cx, |s, cx| on_pick(s, ix.row, window, cx))
                    .ok();
            },
        ))
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

/// Grouped the way people reach for them: workspace, chat, plan, files and diff, git, sections,
/// view, app.
pub(super) const COMMANDS: &[Row] = &[
    Row(
        "New workspace…",
        IconName::Plus,
        Some(&NewWorkspace),
        "create branch",
        |s, w, cx| s.new_workspace(s.state.last_repos.clone(), w, cx),
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
        "Open in Terminal",
        IconName::SquareTerminal,
        Some(&OpenInTerminal),
        "workspace shell",
        |s, w, cx| s.with_target(w, cx, |ws, _, _| open_terminal(&ws.dir())),
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
        "forget claude session reset",
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
        |s, w, cx| s.show(Tab::Diff, w, cx),
    ),
    Row(
        "Open file in editor",
        IconName::FilePen,
        Some(&OpenFile),
        "",
        |s, w, cx| {
            if let Some(ix) = s.cursor_file(w, cx) {
                s.open_file(ix, w, cx)
            }
        },
    ),
    Row(
        "Toggle file viewed",
        IconName::Eye,
        Some(&ToggleViewed),
        "mark unviewed",
        |s, w, cx| {
            if let Some(ix) = s.cursor_file(w, cx) {
                s.toggle_viewed(ix, cx)
            }
        },
    ),
    Row(
        "Comment on diff line",
        IconName::MessageSquarePlus,
        Some(&AddComment),
        "review",
        |s, w, cx| s.comment_dialog(w, cx),
    ),
    Row(
        "Send diff comments to agent",
        IconName::Send,
        Some(&SendComments),
        "review",
        |s, w, cx| s.send_comments(w, cx),
    ),
    Row(
        "Save file",
        IconName::Save,
        Some(&SaveFile),
        "",
        |s, w, cx| {
            if let Some(e) = s.editor.clone() {
                e.update(cx, |e, cx| e.save(false, w, cx))
            }
        },
    ),
    Row(
        "Close file",
        IconName::X,
        Some(&CloseFile),
        "editor",
        |s, w, cx| s.close_file(w, cx),
    ),
    Row(
        "Run next git step",
        IconName::GitPullRequestArrow,
        Some(&GitNext),
        "git commit push",
        |s, w, cx| s.with_sync(w, cx, GitSync::primary),
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
    Row("Pull", IconName::ArrowDown, None, "git fast-forward", |s, w, cx| {
        s.with_sync(w, cx, |g, w, cx| g.run_kind(sync::Kind::Pull, "Pull", w, cx))
    }),
    Row("Push", IconName::ArrowUp, None, "git", |s, w, cx| {
        s.with_sync(w, cx, |g, w, cx| g.run_kind(sync::Kind::Push, "Push", w, cx))
    }),
    Row(
        "Update from base",
        IconName::GitCompareArrows,
        None,
        "git rebase merge main agent",
        |s, w, cx| s.with_sync(w, cx, GitSync::update_from_base),
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
        "Open PRs in browser",
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
        |_, w, cx| sync::toggle_delete_remote(w, cx),
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
        "Toggle sidebar",
        IconName::PanelLeft,
        Some(&ToggleSidebar),
        "rail",
        |s, _, cx| s.toggle_sidebar(cx),
    ),
    Row(
        "Toggle file tree",
        IconName::ListTree,
        Some(&ToggleFileTree),
        "files",
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
        "About Shark",
        IconName::Info,
        None,
        "version",
        |_, w, cx| crate::settings::about(w, cx),
    ),
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

impl Shark {
    pub(super) fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Sidebar order with folded sections opened; cmd-n counts the shown rows only.
        let order = self.ws_order(true);
        let shown = self.ws_order(false);
        let items: Vec<CommandItem> = COMMANDS
            .iter()
            .map(|Row(label, icon, action, words, _)| {
                let key = action.and_then(|a| shortcut(a, window));
                row_item(*label, *icon, key).keywords(words.split_whitespace())
            })
            .chain(order.iter().map(|&ix| {
                let key = (shown.iter().position(|&s| s == ix))
                    .filter(|&n| n < 9)
                    .and_then(|n| shortcut(&OpenWorkspace(n), window));
                let name = &self.state.workspaces[ix].name;
                row_item(format!("Open workspace {name}"), IconName::GitBranch, key)
                    .keywords(["go", "switch"])
            }))
            .collect();
        open_menu(
            cx.entity().downgrade(),
            "Type a command or search…",
            items,
            0,
            move |s, row, window, cx| match COMMANDS.get(row) {
                Some(Row(.., f)) => f(s, window, cx),
                None => s.open_workspace(order[row - COMMANDS.len()], window, cx),
            },
            window,
            cx,
        );
    }
}
