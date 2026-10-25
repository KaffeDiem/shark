//! The cmd-k palette and the cmd-k style menus built on it.
use super::*;

/// The cmd-k bar look: a bare 640px dialog near the top.
pub(super) fn menu_dialog(d: Dialog) -> Dialog {
    d.w(px(640.))
        .font_features(inter_features())
        .margin_top(px(96.))
        .entrance(DialogEntrance::None)
        .close_button(false)
}

/// Opens a cmd-k style menu over `target`; `on_pick` gets the confirmed row.
pub(crate) fn open_menu<T: 'static>(
    target: WeakEntity<T>,
    items: Vec<CommandItem>,
    selected: usize,
    on_pick: impl Fn(&mut T, usize, &mut Window, &mut Context<T>) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let palette = cx.new(|cx| CommandState::new(window, cx));
    // The menu only learns its items when it renders; install them now so the
    // highlight can start on `selected`. The dialog's renders keep it.
    RenderOnce::render(Command::new(&palette).items(items.clone()), window, cx);
    palette.update(cx, |p, cx| {
        p.set_selected_index(Some(IndexPath::new(selected)), window, cx)
    });
    let on_pick = Rc::new(on_pick);
    let state = palette.clone();
    window.open_dialog(cx, move |d, _, _| {
        let (target, on_pick) = (target.clone(), on_pick.clone());
        menu_dialog(d).child(Command::new(&state).items(items.clone()).on_confirm(
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

pub(super) const COMMANDS: &[(&str, IconName, Cmd)] = &[
    ("New workspace", IconName::Plus, |s, w, cx| {
        s.new_workspace(s.state.last_repos.clone(), w, cx)
    }),
    ("Add repo", IconName::FolderPlus, |s, w, cx| {
        s.add_repo(w, cx)
    }),
    (
        "Add repo to workspace…",
        IconName::FolderPlus,
        |s, w, cx| s.pick_repo_for_workspace(w, cx),
    ),
    ("Archive workspace", IconName::Archive, |s, w, cx| {
        s.archive(false, w, cx)
    }),
    (
        "Archive workspace (force, discards changes)",
        IconName::ArchiveX,
        |s, w, cx| s.archive(true, w, cx),
    ),
    (
        "Reveal workspace in Finder",
        IconName::FolderOpen,
        |s, w, cx| s.with_target(w, cx, |ws, _, cx| cx.reveal_path(&ws.dir())),
    ),
    (
        "Open workspace in Terminal",
        IconName::SquareTerminal,
        |s, w, cx| s.with_target(w, cx, |ws, _, _| open_terminal(&ws.dir())),
    ),
    ("Copy workspace path", IconName::Copy, |s, w, cx| {
        s.with_target(w, cx, |ws, w, cx| {
            copy(ws.dir().display().to_string(), w, cx)
        })
    }),
    ("Copy branch name", IconName::GitBranch, |s, w, cx| {
        s.with_target(w, cx, |ws, w, cx| copy(ws.branch.clone(), w, cx))
    }),
    ("Show chat", IconName::MessageSquare, |s, w, cx| {
        s.show(Tab::Chat, w, cx)
    }),
    ("New chat", IconName::Plus, |s, w, cx| s.new_chat(w, cx)),
    ("Close chat", IconName::X, |s, w, cx| s.close_chat(w, cx)),
    (
        "Reopen closed chat…",
        IconName::ArchiveRestore,
        |s, w, cx| s.pick_closed_chat(w, cx),
    ),
    ("Next chat", IconName::ChevronRight, |s, w, cx| {
        s.cycle_chat(1, w, cx)
    }),
    ("Previous chat", IconName::ChevronLeft, |s, w, cx| {
        s.cycle_chat(-1, w, cx)
    }),
    (
        "Ask a side question",
        IconName::MessageCircleQuestionMark,
        |s, w, cx| s.open_side(w, cx),
    ),
    ("Show diff", IconName::FileDiff, |s, w, cx| {
        s.show(Tab::Diff, w, cx)
    }),
    (
        "Create / update linked PRs",
        IconName::GitPullRequest,
        |s, w, cx| s.create_prs(w, cx),
    ),
    ("Open PRs in browser", IconName::ExternalLink, |s, w, cx| {
        s.with_sync(w, cx, GitSync::open_prs)
    }),
    (
        "Create draft PRs",
        IconName::GitPullRequestDraft,
        |s, w, cx| s.create_draft_prs(w, cx),
    ),
    (
        "Move repo earlier in merge order…",
        IconName::ArrowUp,
        |s, w, cx| s.with_sync(w, cx, |g, w, cx| g.move_in_merge_order(true, w, cx)),
    ),
    (
        "Move repo later in merge order…",
        IconName::ArrowDown,
        |s, w, cx| s.with_sync(w, cx, |g, w, cx| g.move_in_merge_order(false, w, cx)),
    ),
    (
        "Toggle deleting remote branches after merge",
        IconName::GitBranch,
        |_, w, cx| sync::toggle_delete_remote(w, cx),
    ),
    (
        "Git: next step",
        IconName::GitPullRequestArrow,
        |s, w, cx| s.with_sync(w, cx, GitSync::primary),
    ),
    ("Git menu…", IconName::GitBranch, |s, w, cx| {
        s.with_sync(w, cx, GitSync::open_menu)
    }),
    ("Git: fetch now", IconName::RefreshCw, |s, w, cx| {
        s.with_sync(w, cx, GitSync::fetch_now)
    }),
    ("New section", IconName::FolderPlus, |s, w, cx| {
        s.start_naming(None, None, w, cx)
    }),
    (
        "Move workspace to section…",
        IconName::FolderInput,
        |s, w, cx| s.pick_section(w, cx),
    ),
    ("Rename section", IconName::Pencil, |s, w, cx| {
        s.with_section(w, cx, |s, i, w, cx| s.start_naming(Some(i), None, w, cx))
    }),
    ("Change section icon…", IconName::Shapes, |s, w, cx| {
        s.with_section(w, cx, Shark::pick_icon)
    }),
    (
        "Collapse / expand section",
        IconName::ChevronsUpDown,
        |s, w, cx| s.with_section(w, cx, |s, i, _, cx| s.toggle_section(i, cx)),
    ),
    (
        "Delete section (its workspaces stay)",
        IconName::Trash,
        |s, w, cx| s.with_section(w, cx, Shark::delete_section),
    ),
    ("Toggle sidebar", IconName::PanelLeft, |s, _, cx| {
        s.toggle_sidebar(cx)
    }),
    ("Toggle file tree", IconName::ListTree, |s, w, cx| {
        s.toggle_tree(w, cx)
    }),
    ("Mark file viewed / unviewed", IconName::Eye, |s, w, cx| {
        if let Some(ix) = s.cursor_file(w, cx) {
            s.toggle_viewed(ix, cx)
        }
    }),
    ("Open file in editor", IconName::FilePen, |s, w, cx| {
        if let Some(ix) = s.cursor_file(w, cx) {
            s.open_file(ix, w, cx)
        }
    }),
    ("Open file…", IconName::FileSearch, |s, w, cx| {
        s.open_any_file(w, cx)
    }),
    (
        "Comment on the diff line",
        IconName::MessageSquarePlus,
        |s, w, cx| s.comment_dialog(w, cx),
    ),
    (
        "Send diff comments to the agent",
        IconName::Send,
        |s, w, cx| s.send_comments(w, cx),
    ),
    ("Save file", IconName::Save, |s, w, cx| {
        if let Some(e) = s.editor.clone() {
            e.update(cx, |e, cx| e.save(false, w, cx))
        }
    }),
    ("Close file", IconName::X, |s, w, cx| s.close_file(w, cx)),
    ("Toggle Vim keys", IconName::Keyboard, |s, w, cx| {
        s.toggle_vim(w, cx)
    }),
    ("Interrupt agent", IconName::Square, |s, _, cx| s.stop(cx)),
    (
        "New conversation (forget the claude session)",
        IconName::MessageSquarePlus,
        |s, w, cx| match s.active_chat().cloned() {
            Some(chat) => chat.update(cx, |c, cx| c.new_conversation(cx)),
            None => notify_err(w, cx, "Open a workspace first"),
        },
    ),
    (
        "Manage queued messages",
        IconName::ListOrdered,
        |s, w, cx| s.focus_queue(w, cx),
    ),
    ("Change model…", IconName::Sparkles, |s, w, cx| {
        s.pick_model(w, cx)
    }),
    ("Toggle plan mode", IconName::ClipboardList, |s, w, cx| {
        s.with_plan(w, cx, |c, _, cx| c.set_plan_mode(!c.meta.plan, cx))
    }),
    ("Show or hide plan", IconName::ClipboardList, |s, w, cx| {
        s.with_plan(w, cx, |c, w, cx| c.toggle_plan(w, cx))
    }),
    ("Approve plan and build", IconName::Hammer, |s, w, cx| {
        s.with_plan(w, cx, |c, w, cx| c.approve_plan(w, cx))
    }),
    ("Approve plan and build in a new chat", IconName::Hammer, |s, w, cx| {
        s.build_plan_in_new_chat(w, cx)
    }),
    (
        "Next workspace needing attention",
        IconName::CircleAlert,
        |s, w, cx| s.next_attention(w, cx),
    ),
    ("Settings…", IconName::Settings, |s, w, cx| {
        crate::settings::open(&s.state, w, cx)
    }),
];

impl Shark {
    pub(super) fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items: Vec<CommandItem> = COMMANDS
            .iter()
            .map(|(label, icon, _)| CommandItem::new().label(*label).icon(*icon))
            .chain(self.state.workspaces.iter().map(|w| {
                CommandItem::new()
                    .label(format!("Open workspace: {}", w.name))
                    .icon(IconName::GitBranch)
            }))
            .collect();
        open_menu(
            cx.entity().downgrade(),
            items,
            0,
            |s, row, window, cx| match COMMANDS.get(row) {
                Some((_, _, f)) => f(s, window, cx),
                None => s.open_workspace(row - COMMANDS.len(), window, cx),
            },
            window,
            cx,
        );
    }
}
