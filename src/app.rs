//! Root view: sidebar of repos and workspaces, Chat/Diff tabs, palette and dialogs.
use crate::attention::Attn;
use crate::chat::{Chat, ChatEvent, MODELS};
use crate::editor::{EditorEvent, FileEditor};
use crate::files::{self, Row};
use crate::state::{ChatMeta, Repo, Section, State, Workspace, move_item, workspaces_dir};
use crate::sync::{GitSync, SyncEvent};
use crate::*;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::dialog::{Dialog, DialogEntrance};
use gpui_kit::component::diff::{Diff, DiffFile, DiffFileStatus, DiffMode, DiffState};
use gpui_kit::component::hover_card::HoverCard;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::link::Link;
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme, Icon, IndexPath, Sizable as _, Theme, WindowExt, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

mod picker;
mod review;

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

/// `/Users/me/x` → `~/x`: paths in menus are read, so keep them short.
fn tilde(path: &Path) -> String {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    match path.strip_prefix(&home) {
        Ok(rest) if !home.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => path.display().to_string(),
    }
}

// ponytail: always Terminal.app; add a setting if people want iTerm/Ghostty/Warp.
fn open_terminal(path: &Path) {
    let _ = std::process::Command::new("open")
        .args(["-a", "Terminal"])
        .arg(path)
        .spawn();
}

fn copy(text: String, window: &mut Window, cx: &mut App) {
    window.push_notification(format!("Copied {text}"), cx);
    cx.write_to_clipboard(ClipboardItem::new_string(text));
}

/// "Workspace folder" plus one row per repo; `f` gets the picked path.
fn path_submenu(
    menu: PopupMenu,
    icon: IconName,
    label: &'static str,
    paths: &[(String, PathBuf)],
    f: fn(&Path, &mut Window, &mut App),
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let paths = paths.to_vec();
    menu.submenu_with_icon(Some(icon.into()), label, window, cx, move |mut m, _, _| {
        for (ix, (name, path)) in paths.iter().enumerate() {
            if ix == 1 {
                m = m.separator();
            }
            let path = path.clone();
            m = m.item(
                PopupMenuItem::new(format!("{name} — {}", tilde(&path)))
                    .on_click(move |_, window, cx| f(&path, window, cx)),
            );
        }
        m
    })
}

/// The cmd-k bar look: a bare 640px dialog near the top.
fn menu_dialog(d: Dialog) -> Dialog {
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

/// Right-click menu of a sidebar workspace. Shows where everything lives on disk,
/// and keeps the destructive items last. Shortcuts live in the palette, not here (Apple HIG).
fn ws_menu(
    menu: PopupMenu,
    ws: &Workspace,
    ix: usize,
    shark: &WeakEntity<Shark>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let paths: Vec<(String, PathBuf)> = std::iter::once(("Workspace folder".into(), ws.dir()))
        .chain(
            ws.repos
                .iter()
                .map(|r| (r.name.clone(), ws.dir().join(&r.name))),
        )
        .collect();
    let pr_urls = shark
        .upgrade()
        .and_then(|s| Some(s.read(cx).syncs.get(&ws.name)?.read(cx).pr_urls()))
        .unwrap_or_default();
    let on = |f: fn(&mut Shark, String, &mut Window, &mut Context<Shark>)| {
        let (shark, name) = (shark.clone(), ws.name.clone());
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            shark
                .update(cx, |s, cx| f(s, name.clone(), window, cx))
                .ok();
        }
    };
    let branch = ws.branch.clone();
    let shark2 = shark.clone();
    let menu = menu
        .label(format!("Branch {}", ws.branch))
        .separator()
        .item(
            PopupMenuItem::new("Open Workspace")
                .icon(IconName::MessageSquare)
                .on_click(move |_, window, cx| {
                    shark2
                        .update(cx, |s, cx| s.open_workspace(ix, window, cx))
                        .ok();
                }),
        )
        .separator();
    let menu = move_submenu(menu, &ws.name, shark, window, cx).separator();
    let menu = path_submenu(
        menu,
        IconName::FolderOpen,
        "Reveal in Finder",
        &paths,
        |p, _, cx| cx.reveal_path(p),
        window,
        cx,
    );
    let menu = path_submenu(
        menu,
        IconName::SquareTerminal,
        "Open in Terminal",
        &paths,
        |p, _, _| open_terminal(p),
        window,
        cx,
    );
    let menu = path_submenu(
        menu,
        IconName::Copy,
        "Copy Path",
        &paths,
        |p, w, cx| copy(p.display().to_string(), w, cx),
        window,
        cx,
    )
    .item(
        PopupMenuItem::new("Copy Branch Name")
            .icon(IconName::GitBranch)
            .on_click(move |_, w, cx| copy(branch.clone(), w, cx)),
    )
    .when(!pr_urls.is_empty(), |m| {
        m.separator().item(
            PopupMenuItem::new(match pr_urls.len() {
                1 => "Open Pull Request".to_string(),
                n => format!("Open {n} Pull Requests"),
            })
            .icon(IconName::ExternalLink)
            .on_click(move |_, _, cx| pr_urls.iter().for_each(|u| cx.open_url(u))),
        )
    });
    menu.separator()
        .item(
            PopupMenuItem::new("Archive")
                .icon(IconName::Archive)
                .on_click(on(|s, name, w, cx| s.archive_ws(name, false, w, cx))),
        )
        .item(
            PopupMenuItem::new("Force Archive…")
                .icon(IconName::ArchiveX)
                .on_click(on(|s, name, w, cx| s.archive_ws(name, true, w, cx))),
        )
}

/// Icons a section can have, by Lucide name (what `Section::icon` stores).
const SECTION_ICONS: &[(&str, IconName)] = &[
    ("folder", IconName::Folder),
    ("star", IconName::Star),
    ("briefcase", IconName::Briefcase),
    ("bug", IconName::Bug),
    ("rocket", IconName::Rocket),
    ("flame", IconName::Flame),
    ("zap", IconName::Zap),
    ("heart", IconName::Heart),
    ("bookmark", IconName::Bookmark),
    ("flag", IconName::Flag),
    ("tag", IconName::Tag),
    ("inbox", IconName::Inbox),
    ("clock", IconName::Clock),
    ("calendar", IconName::Calendar),
    ("users", IconName::Users),
    ("code", IconName::Code),
    ("wrench", IconName::Wrench),
    ("flask-conical", IconName::FlaskConical),
    ("lightbulb", IconName::Lightbulb),
    ("layers", IconName::Layers),
    ("box", IconName::Box),
    ("globe", IconName::Globe),
    ("shield", IconName::Shield),
    ("sparkles", IconName::Sparkles),
    ("target", IconName::Target),
    ("palette", IconName::Palette),
    ("coffee", IconName::Coffee),
    ("book-open", IconName::BookOpen),
];

fn section_icon(name: &str) -> IconName {
    SECTION_ICONS
        .iter()
        .find(|(n, _)| *n == name)
        .map_or(IconName::Folder, |(_, i)| *i)
}

/// "Move to Section" in a workspace's right-click menu; ends with "New Section…", which moves it there.
fn move_submenu(
    menu: PopupMenu,
    ws: &str,
    shark: &WeakEntity<Shark>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let Some(sh) = shark.upgrade() else {
        return menu;
    };
    let sections: Vec<Section> = sh.read(cx).state.sections.clone();
    let current = sh.read(cx).section_of(ws);
    let (ws, shark) = (ws.to_string(), shark.clone());
    menu.submenu_with_icon(
        Some(IconName::FolderInput.into()),
        "Move to Section",
        window,
        cx,
        move |mut m, _, _| {
            let to = |to: Option<usize>| {
                let (shark, ws) = (shark.clone(), ws.clone());
                move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                    shark
                        .update(cx, |s, cx| s.move_to_section(&ws, to, cx))
                        .ok();
                }
            };
            for (i, sec) in sections.iter().enumerate() {
                m = m.item(
                    PopupMenuItem::new(sec.name.clone())
                        .icon(section_icon(&sec.icon))
                        .checked(current == Some(i))
                        .on_click(to(Some(i))),
                );
            }
            if current.is_some() {
                m = m.item(
                    PopupMenuItem::new("No Section")
                        .icon(IconName::FolderOutput)
                        .on_click(to(None)),
                );
            }
            if !sections.is_empty() {
                m = m.separator();
            }
            let (shark, ws) = (shark.clone(), ws.clone());
            m.item(
                PopupMenuItem::new("New Section…")
                    .icon(IconName::FolderPlus)
                    .on_click(move |_, window, cx| {
                        shark
                            .update(cx, |s, cx| {
                                s.start_naming(None, Some(ws.clone()), window, cx)
                            })
                            .ok();
                    }),
            )
        },
    )
}

/// Right-click menu of a section header.
fn section_menu(menu: PopupMenu, i: usize, name: &str, shark: &WeakEntity<Shark>) -> PopupMenu {
    let on = |f: fn(&mut Shark, usize, &mut Window, &mut Context<Shark>)| {
        let shark = shark.clone();
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            shark.update(cx, |s, cx| f(s, i, window, cx)).ok();
        }
    };
    menu.label(format!("Section {name}"))
        .separator()
        .item(
            PopupMenuItem::new("Rename")
                .icon(IconName::Pencil)
                .on_click(on(|s, i, w, cx| s.start_naming(Some(i), None, w, cx))),
        )
        .item(
            PopupMenuItem::new("Change Icon…")
                .icon(IconName::Shapes)
                .on_click(on(|s, i, w, cx| s.pick_icon(i, w, cx))),
        )
        .item(
            PopupMenuItem::new("New Section")
                .icon(IconName::FolderPlus)
                .on_click(on(|s, _, w, cx| s.start_naming(None, None, w, cx))),
        )
        .separator()
        .item(
            PopupMenuItem::new("Delete Section")
                .icon(IconName::Trash)
                .on_click(on(|s, i, w, cx| s.delete_section(i, w, cx))),
        )
}

/// A sidebar row the cursor can sit on.
#[derive(PartialEq, Clone, Copy, Debug)]
enum SideRow {
    /// Index into `state.sections`.
    Section(usize),
    /// Index into `state.workspaces`.
    Ws(usize),
}

/// The inline section name field in the sidebar.
struct Naming {
    /// The section being renamed; `None` adds one.
    rename: Option<usize>,
    /// Workspace to move into the new section ("Move to Section › New Section…").
    then_move: Option<String>,
    input: Entity<InputState>,
    _sub: Subscription,
}

/// A workspace row being dragged: onto a header to move it in, onto a row to take its slot.
#[derive(Clone)]
struct DraggedWs(SharedString);

/// A section header being dragged onto another to take its place.
#[derive(Clone)]
struct DraggedSection(usize, SharedString);

impl Render for DraggedWs {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        drag_chip(self.0.clone(), cx)
    }
}

impl Render for DraggedSection {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        drag_chip(self.1.clone(), cx)
    }
}

fn drag_chip(label: SharedString, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .px_2()
        .py_1()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.secondary)
        .text_sm()
        .text_color(theme.foreground)
        .font_features(inter_features())
        .child(label)
}

#[derive(PartialEq, Clone, Copy, Debug)]
enum Tab {
    Chat,
    Diff,
    /// The open file next to the chat; only while `editor` is set.
    File,
}

/// Focusable columns, left to right; `ctrl-w h/l` walks them.
#[derive(PartialEq, Clone, Copy, Debug)]
enum Pane {
    Sidebar,
    Chat,
    Diff,
    Editor,
    Tree,
}

/// A changed file against the fingerprint stored when it was marked viewed.
#[derive(PartialEq, Clone, Copy, Debug)]
enum Viewed {
    No,
    Yes,
    /// Marked viewed, then the agent or the editor changed it.
    Changed,
}

/// One parsed diff refresh, built off the main thread.
struct DiffData {
    /// Fingerprint of the whole patch; equal means nothing to redo.
    fp: u64,
    /// In tree order.
    files: Vec<DiffFile>,
    fps: Vec<u64>,
    /// git failed for some repo; the patch is then empty, which must not read as "no changes".
    error: Option<String>,
    hunks: HashMap<SharedString, Vec<review::Hunk>>,
}

impl DiffData {
    fn load(ws: &Workspace) -> DiffData {
        let (patch, error) = match git::combined_diff(ws, &ws.dir()) {
            Ok(p) => (p, None),
            Err(e) => (String::new(), Some(e)),
        };
        let fp = files::fingerprint(&patch);
        let mut hunks = HashMap::new();
        let mut parsed: Vec<(DiffFile, u64)> = files::split_patch(&patch)
            .into_iter()
            .filter_map(|chunk| {
                let file = DiffFile::parse(chunk).ok()?.into_iter().next()?;
                hunks.insert(file.path().clone(), review::parse_hunks(chunk));
                Some((file, files::fingerprint(chunk)))
            })
            .collect();
        let paths: Vec<String> = parsed.iter().map(|(f, _)| f.path().to_string()).collect();
        let order = files::tree_order(&paths);
        let mut slots: Vec<Option<(DiffFile, u64)>> = parsed.drain(..).map(Some).collect();
        let (files, fps) = order.into_iter().filter_map(|i| slots[i].take()).unzip();
        DiffData {
            fp,
            files,
            fps,
            error,
            hunks,
        }
    }
}

pub struct Shark {
    pub(crate) state: State,
    sel: SideRow,
    naming: Option<Naming>,
    pub(crate) active: Option<String>,
    /// By chat id; created when a chat is first shown.
    pub(crate) chats: HashMap<String, Entity<Chat>>,
    /// Chats whose turn ended while another chat was shown.
    unread: HashSet<String>,
    tab: Tab,
    diff: Entity<DiffState>,
    /// Changed files in tree order, with each patch's fingerprint.
    diff_files: Vec<DiffFile>,
    diff_fps: Vec<u64>,
    diff_fp: u64,
    /// Bumped per refresh, so a slow older refresh can't overwrite a newer one.
    diff_gen: u64,
    /// The file last moved to (`}`/`{`, the tree); the diff can't say which one is on screen.
    diff_file: usize,
    /// False until the first refresh of the open workspace lands.
    diff_loaded: bool,
    diff_error: Option<String>,
    /// Paths currently viewed (fingerprint still matches), for the diff headers.
    viewed_now: Rc<HashSet<SharedString>>,
    tree_rows: Vec<Row>,
    collapsed_dirs: HashSet<String>,
    tree_sel: usize,
    tree_focus: FocusHandle,
    tree_scroll: UniformListScrollHandle,
    sidebar_scroll: ScrollHandle,
    /// Workspaces whose worktrees are being removed in the background.
    pub(crate) archiving: HashSet<String>,
    show_tree: bool,
    /// Sidebar shrunk to icons; `rail_auto` when opening a file did it, so closing undoes it.
    rail: bool,
    rail_auto: bool,
    editor: Option<Entity<FileEditor>>,
    /// What the editor header shows, to redraw only when it changes, not on every key.
    editor_header: (bool, bool, &'static str, String),
    _editor_subs: Vec<Subscription>,
    review: review::Review,
    pub(crate) syncs: HashMap<String, Entity<GitSync>>,
    pub(crate) sidebar_focus: FocusHandle,
    pub(crate) attn: crate::attention::Attention,
    _subs: Vec<Subscription>,
}

type Cmd = fn(&mut Shark, &mut Window, &mut Context<Shark>);

const COMMANDS: &[(&str, IconName, Cmd)] = &[
    ("New workspace", IconName::Plus, |s, w, cx| {
        s.new_workspace(w, cx)
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
        s.open_prs(w, cx)
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
            None => Shark::notify_err(w, cx, "Open a workspace first"),
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
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = State::load();
        crate::prompts::set_overrides(&state.prompts);
        crate::settings::apply(&state.appearance, cx);
        let sidebar_focus = cx.focus_handle();
        sidebar_focus.focus(window, cx);
        cx.defer_in(window, |s, window, cx| s.restore_last_open(window, cx));
        let _subs = vec![
            cx.observe_window_appearance(window, |s, _, cx| {
                if s.state.appearance.mode == crate::state::Mode::System {
                    crate::settings::apply(&s.state.appearance, cx)
                }
            }),
            cx.observe_window_activation(window, |s, w, _| {
                s.attn.window_active = w.is_window_active()
            }),
        ];
        Shark {
            state,
            sel: SideRow::Ws(0),
            naming: None,
            active: None,
            chats: HashMap::new(),
            unread: HashSet::new(),
            tab: Tab::Chat,
            diff: cx.new(|cx| DiffState::new(vec![], cx)),
            diff_files: vec![],
            diff_fps: vec![],
            diff_fp: 0,
            diff_gen: 0,
            diff_file: 0,
            diff_loaded: false,
            diff_error: None,
            viewed_now: Rc::default(),
            tree_rows: vec![],
            collapsed_dirs: HashSet::new(),
            tree_sel: 0,
            tree_focus: cx.focus_handle(),
            tree_scroll: UniformListScrollHandle::new(),
            sidebar_scroll: ScrollHandle::new(),
            archiving: HashSet::new(),
            show_tree: true,
            rail: false,
            rail_auto: false,
            editor: None,
            editor_header: Default::default(),
            _editor_subs: vec![],
            review: Default::default(),
            syncs: HashMap::new(),
            sidebar_focus,
            attn: crate::attention::Attention::new(cx),
            _subs,
        }
    }

    fn notify_err(window: &mut Window, cx: &mut App, e: impl Into<SharedString>) {
        window.push_notification(Notification::error(e), cx);
    }

    fn active_ws(&self) -> Option<&Workspace> {
        let name = self.active.as_ref()?;
        self.state.workspaces.iter().find(|w| &w.name == name)
    }

    fn active_ws_mut(&mut self) -> Option<&mut Workspace> {
        let name = self.active.clone()?;
        self.state.workspaces.iter_mut().find(|w| w.name == name)
    }

    fn active_chat(&self) -> Option<&Entity<Chat>> {
        let ws = self.active_ws()?;
        self.chats.get(&ws.chats.get(ws.active_chat)?.id)
    }

    /// The chat entity for `meta`, started (transcript replayed) on first use.
    fn ensure_chat(
        &mut self,
        ws: &Workspace,
        meta: &ChatMeta,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<Chat> {
        if let Some(chat) = self.chats.get(&meta.id) {
            return chat.clone();
        }
        // Opened mid-archive (its row is still listed): sends stay refused.
        let archiving = self.archiving.contains(&ws.name);
        let chat = cx.new(|cx| {
            let mut c = Chat::new(ws.clone(), meta.clone(), window, cx);
            c.archiving = archiving;
            c
        });
        let (id, name) = (meta.id.clone(), ws.name.clone());
        // Busy also flips without a ChatEvent (background tasks, turns the CLI starts itself).
        self._subs.push(cx.observe(&chat, |_, _, cx| cx.notify()));
        self._subs.push(
            cx.subscribe(&chat, move |this, chat, ev: &ChatEvent, cx| match ev {
                ChatEvent::Meta => this.save_meta(&chat, cx),
                ChatEvent::TurnDone => {
                    if this.active.as_ref() == Some(&name) {
                        this.refresh_diff(cx);
                    }
                    if this.active_chat() != Some(&chat) {
                        this.unread.insert(id.clone());
                    }
                    this.turn_done(&name, cx);
                    cx.notify();
                }
            }),
        );
        if let Some(sync) = self.syncs.get(&ws.name) {
            sync.update(cx, |g, cx| g.add_chat(chat.clone(), cx));
        }
        self.chats.insert(meta.id.clone(), chat.clone());
        self.save_meta(&chat, cx); // replay may have found a title
        chat
    }

    fn save_meta(&mut self, chat: &Entity<Chat>, cx: &mut Context<Self>) {
        let meta = chat.read(cx).meta.clone();
        let slot = (self.state.workspaces.iter_mut())
            .flat_map(|w| w.chats.iter_mut())
            .find(|c| c.id == meta.id);
        if let Some(slot) = slot
            && *slot != meta
        {
            *slot = meta;
            self.state.save();
        }
    }

    /// Shows the workspace's `ix`th chat tab.
    fn select_chat(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.active_ws_mut() else {
            return;
        };
        let Some(meta) = ws.chats.get(ix).cloned() else {
            return;
        };
        if ws.active_chat != ix {
            ws.active_chat = ix;
            self.state.save();
        }
        let ws = self.active_ws().cloned().unwrap();
        let chat = self.ensure_chat(&ws, &meta, window, cx);
        self.unread.remove(&meta.id);
        if let Some(sync) = self.syncs.get(&ws.name) {
            sync.update(cx, |g, cx| g.set_chat(chat.clone(), cx));
        }
        if self.tab == Tab::Diff {
            self.tab = Tab::Chat;
        }
        chat.update(cx, |c, cx| c.focus_composer(window, cx));
        cx.notify();
    }

    fn cycle_chat(&mut self, d: isize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ws) = self.active_ws() {
            let n = ws.chats.len() as isize;
            let ix = (ws.active_chat as isize + d).rem_euclid(n) as usize;
            self.select_chat(ix, window, cx);
        }
    }

    /// cmd-t: a new tab right of the current one, on the current chat's model.
    fn new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let model = self
            .active_chat()
            .and_then(|c| c.read(cx).meta.model.clone());
        let Some(ws) = self.active_ws_mut() else {
            return Self::notify_err(window, cx, "Open a workspace first");
        };
        let meta = ChatMeta {
            id: ws.next_chat_id(),
            model,
            ..Default::default()
        };
        let at = ws.active_chat + 1;
        ws.chats.insert(at, meta);
        self.state.save();
        self.select_chat(at, window, cx);
    }

    /// cmd-w: closes the shown chat, asking first if it's running. The last tab gives way to a
    /// fresh one, so a workspace always has a chat.
    fn close_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = self.active_chat().cloned() else {
            return;
        };
        if !chat.read(cx).working() {
            return self.close_chat_now(window, cx);
        }
        let shark = cx.entity().downgrade();
        let title = chat.read(cx).title();
        window.open_alert_dialog(cx, move |d, _, _| {
            let shark = shark.clone();
            d.font_features(inter_features())
                .title(format!("Stop and close “{title}”?"))
                .description("The agent is still running in this chat. Closing stops it; cmd-shift-t reopens the chat.")
                .ok_text("Stop and close")
                .ok_variant(ButtonVariant::Danger)
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    shark.update(cx, |s, cx| s.close_chat_now(window, cx)).ok();
                    true
                })
        });
    }

    fn close_chat_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.active_ws_mut() else {
            return;
        };
        let ix = ws.active_chat;
        let meta = ws.chats.remove(ix);
        if ws.chats.is_empty() {
            let id = ws.next_chat_id();
            ws.chats.push(ChatMeta {
                id,
                model: meta.model.clone(),
                ..Default::default()
            });
        }
        ws.active_chat = ix.min(ws.chats.len() - 1);
        let next = ws.active_chat;
        let (name, untouched) = (ws.name.clone(), meta.session_id.is_none());
        // An empty chat isn't worth reopening.
        if !untouched {
            ws.closed.push(meta.clone());
        }
        // ponytail: keeps the 20 newest closed chats; a closed-chats browser can lift this
        let drop = ws.closed.len().saturating_sub(20);
        let evicted: Vec<ChatMeta> = ws.closed.drain(..drop).collect();
        self.state.save();
        for m in evicted.iter().chain(untouched.then_some(&meta)) {
            let _ = std::fs::remove_file(crate::agent::transcript_path(&m.id));
        }
        self.unread.remove(&meta.id);
        if let Some(chat) = self.chats.remove(&meta.id)
            && let Some(sync) = self.syncs.get(&name)
        {
            sync.update(cx, |g, cx| g.remove_chat(&chat, cx)); // the last ref; dropping stops claude
        }
        self.select_chat(next, window, cx);
    }

    /// cmd-shift-t: brings back the most recently closed chat, or a picked one.
    fn reopen_chat(&mut self, pick: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.active_ws_mut() else {
            return Self::notify_err(window, cx, "Open a workspace first");
        };
        let Some(ix) = pick.or(ws.closed.len().checked_sub(1)) else {
            return Self::notify_err(window, cx, "No closed chats in this workspace");
        };
        let meta = ws.closed.remove(ix);
        let at = ws.active_chat + 1;
        ws.chats.insert(at, meta);
        self.state.save();
        self.select_chat(at, window, cx);
    }

    fn pick_closed_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.active_ws() else {
            return Self::notify_err(window, cx, "Open a workspace first");
        };
        if ws.closed.is_empty() {
            return Self::notify_err(window, cx, "No closed chats in this workspace");
        }
        // Newest first.
        let n = ws.closed.len();
        let items = (ws.closed.iter().rev())
            .map(|c| {
                let title = if c.title.is_empty() {
                    "Untitled chat"
                } else {
                    &c.title
                };
                CommandItem::new()
                    .label(title.to_string())
                    .icon(IconName::MessageSquare)
            })
            .collect();
        self.open_menu(
            items,
            0,
            move |s, row, w, cx| s.reopen_chat(Some(n - 1 - row), w, cx),
            window,
            cx,
        );
    }

    fn open_side(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = self.active_chat().cloned() else {
            return Self::notify_err(window, cx, "Open a workspace first");
        };
        if self.tab == Tab::Diff {
            self.tab = Tab::Chat;
        }
        chat.update(cx, |c, cx| c.open_side(window, cx));
        cx.notify();
    }

    fn active_sync(&self) -> Option<&Entity<GitSync>> {
        self.syncs.get(self.active.as_ref()?)
    }

    fn with_sync(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut GitSync, &mut Window, &mut Context<GitSync>),
    ) {
        match self.active_sync().cloned() {
            Some(sync) => sync.update(cx, |g, cx| f(g, window, cx)),
            None => Self::notify_err(window, cx, "Open a workspace first"),
        }
    }

    pub(crate) fn open_workspace(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(name) = self.state.workspaces.get(ix).map(|w| w.name.clone()) else {
            return;
        };
        if self.active.as_ref() == Some(&name) || self.editor.is_none() {
            return self.open_workspace_now(ix, window, cx);
        }
        self.confirm_discard(window, cx, move |s, window, cx| {
            s.drop_editor();
            s.open_workspace_now(ix, window, cx)
        });
    }

    fn open_workspace_now(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.state.workspaces.get(ix).cloned() else {
            return;
        };
        if self.active.as_ref() != Some(&ws.name) {
            // A fresh tree and diff for another workspace.
            self.diff_fp = 0;
            self.diff_files.clear();
            self.diff_fps.clear();
            self.viewed_now = Rc::default();
            self.diff_file = 0;
            self.diff_loaded = false;
            self.diff_error = None;
            self.tree_rows.clear();
            self.collapsed_dirs.clear();
            self.tree_sel = 0;
            self.lose_view();
        }
        self.sel = SideRow::Ws(ix);
        if let Some(i) = self.section_of(&ws.name)
            && self.state.sections[i].collapsed
        {
            self.state.sections[i].collapsed = false;
            self.state.save();
        }
        if !self.syncs.contains_key(&ws.name) {
            let sync = cx.new(|cx| GitSync::new(ws.clone(), window, cx));
            let name = ws.name.clone();
            self._subs.push(cx.subscribe_in(
                &sync,
                window,
                move |this, _, ev: &SyncEvent, window, cx| match ev {
                    SyncEvent::Archive => this.remove_workspace(&name, false, window, cx),
                },
            ));
            self.syncs.insert(ws.name.clone(), sync);
        }
        for (name, sync) in &self.syncs {
            let on = *name == ws.name;
            sync.update(cx, |g, cx| g.set_active(on, cx));
        }
        self.attn.opened(&ws.name);
        self.active = Some(ws.name);
        self.refresh_attention(cx);
        self.refresh_diff(cx);
        self.show(Tab::Chat, window, cx);
        self.select_chat(ws.active_chat, window, cx);
    }

    fn show(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = tab;
        self.remember_open();
        match tab {
            Tab::Chat => {
                if let Some(chat) = self.active_chat() {
                    let focus = chat.read(cx).focus.clone();
                    focus.focus(window, cx);
                }
            }
            Tab::Diff => {
                self.refresh_diff(cx);
                self.diff.read(cx).focus_handle(cx).focus(window, cx);
            }
            Tab::File => match &self.editor {
                Some(e) => crate::editor::focus(e, window, cx),
                None => return self.show(Tab::Diff, window, cx),
            },
        }
        cx.notify();
    }

    fn refresh_diff(&mut self, cx: &mut Context<Self>) {
        let Some(ws) = self
            .active
            .as_ref()
            .and_then(|n| self.state.workspaces.iter().find(|w| &w.name == n))
            .cloned()
        else {
            return;
        };
        self.diff_gen += 1;
        let generation = self.diff_gen;
        cx.spawn(async move |this, cx| {
            let name = ws.name.clone();
            let data = cx
                .background_executor()
                .spawn(async move { DiffData::load(&ws) })
                .await;
            this.update(cx, |s, cx| {
                if s.active.as_ref() != Some(&name) || s.diff_gen != generation {
                    return; // superseded: another workspace, or a newer refresh
                }
                s.apply_diff(data, cx);
            })
            .ok();
        })
        .detach();
    }

    fn apply_diff(&mut self, data: DiffData, cx: &mut Context<Self>) {
        let status_changed = !self.diff_loaded || data.error != self.diff_error;
        self.diff_loaded = true;
        self.diff_error = data.error;
        // Turn ends and saves refresh often; an unchanged patch keeps scroll and expansion.
        if data.fp == self.diff_fp && !self.diff_files.is_empty() {
            if status_changed {
                cx.notify();
            }
            return;
        }
        self.review.hunks = data.hunks;
        if let Some(ws) = self.active.clone() {
            self.review.load_comments(&ws);
        }
        let was_viewed = self.viewed_now.clone();
        // set_files scrolls back to the top; come back to the file the user was on.
        let kept = self
            .diff_files
            .get(self.diff_file)
            .map(|f| f.path().clone());
        let tree_on_file = (self.tree_rows.get(self.tree_sel)).is_none_or(|r| r.file.is_some());
        // First files of this workspace's diff: no mouse scroll yet, so `j` starts a line cursor.
        let first_load = self.diff_files.is_empty();
        self.diff_fp = data.fp;
        self.diff_fps = data.fps;
        self.diff_files = data.files;
        self.diff_file = kept
            .and_then(|p| self.diff_files.iter().position(|f| *f.path() == p))
            .unwrap_or(self.diff_file.min(self.diff_files.len().saturating_sub(1)));
        self.rebuild_tree();
        if tree_on_file
            && let Some(row) = (self.tree_rows.iter()).position(|r| r.file == Some(self.diff_file))
        {
            self.tree_sel = row;
        }
        self.recompute_viewed();
        let files = self.diff_files.clone();
        let viewed = self.viewed_now.clone();
        let at = self.diff_file;
        self.diff.update(cx, |d, cx| {
            d.set_files(files.iter().cloned(), cx);
            for f in &files {
                let path = f.path();
                if viewed.contains(path) {
                    d.set_file_collapsed(path, true, cx);
                } else if was_viewed.contains(path) {
                    d.set_file_collapsed(path, false, cx);
                }
            }
        });
        if first_load && let Some(f) = files.get(at) {
            self.review.at_file(f.path());
        }
        // The keyboard view comes back to its line; otherwise the file last moved to.
        if !self.restore_view(cx)
            && let Some(f) = files.get(at)
        {
            self.diff.update(cx, |d, cx| d.scroll_to_file(f.path(), cx));
        }
        cx.notify();
    }

    fn viewed(&self, ix: usize) -> Viewed {
        let (Some(ws), Some(f)) = (self.active_ws(), self.diff_files.get(ix)) else {
            return Viewed::No;
        };
        match ws.viewed.get(f.path().as_str()) {
            None => Viewed::No,
            Some(fp) if *fp == self.diff_fps[ix] => Viewed::Yes,
            Some(_) => Viewed::Changed,
        }
    }

    fn recompute_viewed(&mut self) {
        let set = (0..self.diff_files.len())
            .filter(|&ix| self.viewed(ix) == Viewed::Yes)
            .map(|ix| self.diff_files[ix].path().clone())
            .collect();
        self.viewed_now = Rc::new(set);
    }

    fn toggle_viewed(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.diff_files.len() {
            self.set_viewed(&[ix], self.viewed(ix) != Viewed::Yes, cx);
        }
    }

    /// Files under folder `dir` of the tree.
    fn dir_files(&self, dir: &str) -> Vec<usize> {
        let prefix = format!("{dir}/");
        (0..self.diff_files.len())
            .filter(|&ix| self.diff_files[ix].path().starts_with(&prefix))
            .collect()
    }

    /// A folder's box: ticks every file inside, or unticks them all when all are viewed.
    fn toggle_viewed_dir(&mut self, dir: &str, cx: &mut Context<Self>) {
        let files = self.dir_files(dir);
        let on = !files.iter().all(|&ix| self.viewed(ix) == Viewed::Yes);
        self.set_viewed(&files, on, cx);
    }

    fn set_viewed(&mut self, files: &[usize], on: bool, cx: &mut Context<Self>) {
        let Some(name) = self.active.clone() else {
            return;
        };
        let Some(ws) = self.state.workspaces.iter_mut().find(|w| w.name == name) else {
            return;
        };
        for &ix in files {
            let path = self.diff_files[ix].path().to_string();
            match on {
                true => ws.viewed.insert(path, self.diff_fps[ix]),
                false => ws.viewed.remove(&path),
            };
        }
        // Forget files that left the diff so the map doesn't grow forever.
        let live: HashSet<&str> = self.diff_files.iter().map(|f| f.path().as_str()).collect();
        ws.viewed.retain(|p, _| live.contains(p.as_str()));
        self.state.save();
        self.recompute_viewed();
        let paths: Vec<SharedString> = files
            .iter()
            .map(|&ix| self.diff_files[ix].path().clone())
            .collect();
        // ponytail: the diff rebuilds once per file; add a batch setter to gpui-kit if big folders lag.
        self.diff.update(cx, |d, cx| {
            paths.iter().for_each(|p| d.set_file_collapsed(p, on, cx))
        });
        cx.notify();
    }

    fn toggle_viewed_path(&mut self, path: &str, cx: &mut Context<Self>) {
        if let Some(ix) = self.diff_files.iter().position(|f| f.path() == path) {
            self.toggle_viewed(ix, cx);
        }
    }

    fn rebuild_tree(&mut self) {
        let paths: Vec<String> = self
            .diff_files
            .iter()
            .map(|f| f.path().to_string())
            .collect();
        self.tree_rows = files::rows(&paths, &self.collapsed_dirs);
        self.tree_sel = self.tree_sel.min(self.tree_rows.len().saturating_sub(1));
    }

    fn tree_select(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(r) = self.tree_rows.get(row) else {
            return;
        };
        self.tree_sel = row;
        self.tree_scroll
            .scroll_to_item(row, ScrollStrategy::Nearest);
        if let Some(ix) = r.file {
            self.diff_file = ix;
            if self.tab == Tab::Diff {
                let path = self.diff_files[ix].path().to_string();
                self.review.at_file(&path);
                self.diff.update(cx, |d, cx| d.scroll_to_file(&path, cx));
            }
        }
        cx.notify();
    }

    fn set_dir_open(&mut self, open: Option<bool>, cx: &mut Context<Self>) {
        let Some(r) = self.tree_rows.get(self.tree_sel) else {
            return;
        };
        if r.file.is_some() {
            // `h` on a file jumps to its folder, like most tree views.
            if open == Some(false)
                && let Some(up) = (0..self.tree_sel)
                    .rev()
                    .find(|&i| self.tree_rows[i].depth < r.depth)
            {
                self.tree_select(up, cx);
            }
            return;
        }
        let path = r.path.clone();
        let is_open = !self.collapsed_dirs.contains(&path);
        if open.unwrap_or(!is_open) == is_open {
            return;
        }
        match is_open {
            true => self.collapsed_dirs.insert(path),
            false => self.collapsed_dirs.remove(&path),
        };
        self.rebuild_tree();
        cx.notify();
    }

    /// Enter / click on a tree row: toggle a folder, show a file (in the diff, or the editor
    /// when one is open).
    fn tree_confirm(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.tree_select(row, cx);
        match self.tree_rows.get(row).and_then(|r| r.file) {
            None => self.set_dir_open(None, cx),
            Some(ix) if self.tab == Tab::File => self.open_file(ix, window, cx),
            Some(_) => {}
        }
    }

    /// The file a row action applies to: the tree cursor in the tree, the open file in the file
    /// view, else the file last moved to in the diff.
    fn cursor_file(&self, window: &Window, cx: &App) -> Option<usize> {
        if self.tree_focus.is_focused(window) {
            return self.tree_rows.get(self.tree_sel)?.file;
        }
        if self.tab == Tab::File
            && let Some(e) = &self.editor
        {
            let path = &e.read(cx).path;
            return self.diff_files.iter().position(|f| f.path() == path);
        }
        // The diff can't say which file is on screen; use the last one moved to, which the
        // tree cursor follows.
        (self.diff_file < self.diff_files.len()).then_some(self.diff_file)
    }

    fn open_file(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(f) = self.diff_files.get(ix) else {
            return;
        };
        let path = f.path().to_string();
        let open = (self.editor.as_ref()).is_some_and(|e| e.read(cx).path == path);
        if !open && (f.status() == DiffFileStatus::Deleted || f.is_binary()) {
            return Self::notify_err(
                window,
                cx,
                format!("{path} is deleted or binary; it shows in the diff only"),
            );
        }
        self.open_path(path, window, cx)
    }

    /// Opens `repo/rel/path` of the open workspace in the editor next to the chat.
    fn open_path(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.active_ws() else {
            return;
        };
        if self
            .editor
            .as_ref()
            .is_some_and(|e| e.read(cx).path == path)
        {
            return self.show(Tab::File, window, cx);
        }
        let abs = ws.dir().join(&path);
        if std::fs::metadata(&abs).is_ok_and(|m| m.len() > crate::editor::MAX_BYTES) {
            return Self::notify_err(
                window,
                cx,
                format!("{path} is too large to edit; it shows in the diff only"),
            );
        }
        self.confirm_discard(window, cx, move |s, window, cx| {
            let vim = s.state.vim;
            let editor = cx.new(|cx| FileEditor::new(path, abs, vim, window, cx));
            s._editor_subs = vec![
                cx.subscribe_in(
                    &editor,
                    window,
                    |s, _, ev: &EditorEvent, window, cx| match ev {
                        EditorEvent::Saved => s.refresh_diff(cx),
                        EditorEvent::Leave => {
                            let pane = if s.show_tree { Pane::Tree } else { Pane::Chat };
                            s.focus_pane(pane, window, cx)
                        }
                    },
                ),
                cx.observe(&editor, |s, editor, cx| {
                    let e = editor.read(cx);
                    let header = (
                        e.dirty,
                        e.conflict,
                        e.vim.status(),
                        e.vim.pending().to_string(),
                    );
                    if header != s.editor_header {
                        s.editor_header = header;
                        cx.notify();
                    }
                }),
            ];
            s.editor = Some(editor);
            if !s.rail {
                s.rail = true;
                s.rail_auto = true;
            }
            s.show(Tab::File, window, cx);
        });
    }

    fn close_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor.is_none() {
            return;
        }
        self.confirm_discard(window, cx, |s, window, cx| {
            s.drop_editor();
            if s.tab == Tab::File {
                s.show(Tab::Diff, window, cx);
            }
        });
    }

    fn drop_editor(&mut self) {
        self.editor = None;
        self._editor_subs.clear();
        if self.rail_auto {
            self.rail = false;
            self.rail_auto = false;
        }
    }

    /// Runs `then` now, or after the user agrees to drop unsaved edits in the open file.
    fn confirm_discard(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Shark, &mut Window, &mut Context<Shark>) + 'static,
    ) {
        let Some(path) = self
            .editor
            .as_ref()
            .filter(|e| e.read(cx).dirty)
            .map(|e| e.read(cx).path.clone())
        else {
            return then(self, window, cx);
        };
        let shark = cx.entity().downgrade();
        let then = Rc::new(RefCell::new(Some(then)));
        window.open_alert_dialog(cx, move |d, _, _| {
            let (shark, then) = (shark.clone(), then.clone());
            d.font_features(inter_features())
                .title(format!("Discard changes to {path}?"))
                .description("Your edits are not saved. Cancel and press cmd-s to keep them.")
                .ok_text("Discard")
                .ok_variant(ButtonVariant::Danger)
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    if let Some(f) = then.borrow_mut().take() {
                        shark
                            .update(cx, |s, cx| {
                                if let Some(e) = &s.editor {
                                    e.update(cx, |e, _| e.dirty = false);
                                }
                                f(s, window, cx)
                            })
                            .ok();
                    }
                    true
                })
        });
    }

    fn toggle_vim(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.vim = !self.state.vim;
        self.state.save();
        let on = self.state.vim;
        if let Some(e) = &self.editor {
            e.update(cx, |e, cx| e.set_vim(on, cx));
        }
        window.push_notification(format!("Vim keys {}", if on { "on" } else { "off" }), cx);
        cx.notify();
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.rail = !self.rail;
        self.rail_auto = false;
        cx.notify();
    }

    fn toggle_tree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_tree = !self.show_tree;
        if !self.show_tree && self.tree_focus.is_focused(window) {
            let tab = self.tab;
            self.show(tab, window, cx);
        }
        cx.notify();
    }

    fn panes(&self) -> Vec<Pane> {
        let mut v = vec![Pane::Sidebar];
        match self.tab {
            Tab::Chat => v.push(Pane::Chat),
            Tab::Diff => v.push(Pane::Diff),
            Tab::File => v.extend([Pane::Chat, Pane::Editor]),
        }
        if self.tab != Tab::Chat && self.show_tree && self.active.is_some() {
            v.push(Pane::Tree);
        }
        v
    }

    fn focused_pane(&self, window: &Window, cx: &App) -> Option<Pane> {
        if self.sidebar_focus.is_focused(window) {
            return Some(Pane::Sidebar);
        }
        if self.tree_focus.is_focused(window) {
            return Some(Pane::Tree);
        }
        if self
            .editor
            .as_ref()
            .is_some_and(|e| e.read(cx).is_focused(window, cx))
        {
            return Some(Pane::Editor);
        }
        if let Some(chat) = self.active_chat() {
            let c = chat.read(cx);
            if c.focus.is_focused(window) || c.composer.read(cx).focus_handle(cx).is_focused(window)
            {
                return Some(Pane::Chat);
            }
        }
        self.diff
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
            .then_some(Pane::Diff)
    }

    fn focus_pane(&mut self, pane: Pane, window: &mut Window, cx: &mut Context<Self>) {
        match pane {
            Pane::Sidebar => self.sidebar_focus.focus(window, cx),
            Pane::Tree => self.tree_focus.focus(window, cx),
            Pane::Diff => self.diff.read(cx).focus_handle(cx).focus(window, cx),
            Pane::Chat => {
                if let Some(chat) = self.active_chat() {
                    chat.read(cx).focus.clone().focus(window, cx);
                }
            }
            Pane::Editor => {
                if let Some(e) = &self.editor {
                    crate::editor::focus(e, window, cx);
                }
            }
        }
        cx.notify();
    }

    /// `ctrl-w h` / `ctrl-w l`.
    fn move_focus(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let panes = self.panes();
        let at = self
            .focused_pane(window, cx)
            .and_then(|p| panes.iter().position(|q| *q == p));
        let to = match at {
            Some(i) => (i as isize + delta).clamp(0, panes.len() as isize - 1) as usize,
            None if delta < 0 => 0,
            None => 1.min(panes.len() - 1),
        };
        if panes[to] == Pane::Sidebar || self.active.is_some() {
            self.focus_pane(panes[to], window, cx);
        }
    }

    fn add_repo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("Add repo".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            this.update_in(cx, |s, window, cx| {
                for p in paths {
                    match git::add_repo(&p, &s.state.repos) {
                        Ok(r) => s.state.repos.push(r),
                        Err(e) => Self::notify_err(window, cx, e),
                    }
                }
                s.state.save();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn new_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.repos.is_empty() {
            return Self::notify_err(window, cx, "Add a repo first (space r a)");
        }
        let shark = cx.entity().downgrade();
        let (repos, last) = (self.state.repos.clone(), self.state.last_repos.clone());
        let form = cx.new(|cx| NewWorkspaceForm::new(repos, &last, shark, window, cx));
        let input = form.read(cx).branch.clone();
        window.open_dialog(cx, move |d, _, _| {
            let f = form.clone();
            // Dialog binds enter to Confirm, which runs on_ok and closes when it returns true.
            // In the repo picker, the Command list takes enter first (it ticks a repo).
            d.font_features(inter_features())
                .title("New workspace")
                .w(px(520.))
                .child(form.clone())
                .on_ok(move |_, window, cx| f.update(cx, |f, cx| f.submit(window, cx)))
        });
        input.update(cx, |i, cx| i.focus(window, cx));
    }

    /// `prompt`, when set, is sent to the agent once the workspace is open.
    fn create_workspace(
        &mut self,
        branch: String,
        repos: Vec<Repo>,
        prompt: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = branch.replace('/', "-");
        if self.state.workspaces.iter().any(|w| w.name == name) {
            return Self::notify_err(window, cx, format!("workspace `{name}` already exists"));
        }
        self.state.last_repos = repos.iter().map(|r| r.name.clone()).collect();
        self.state.save();
        window.push_notification(format!("Creating {name}…"), cx);
        cx.spawn_in(window, async move |this, cx| {
            let dir = workspaces_dir().join(&name);
            let result = cx
                .background_executor()
                .spawn(async move { git::create_workspace(&dir, &name, &branch, &repos) })
                .await;
            this.update_in(cx, |s, window, cx| match result {
                Ok(ws) => {
                    let name = ws.name.clone();
                    s.state.workspaces.push(ws);
                    s.state.save();
                    s.open_workspace(s.state.workspaces.len() - 1, window, cx);
                    if let Some(p) = prompt
                        && s.active.as_ref() == Some(&name)
                        && let Some(chat) = s.active_chat().cloned()
                    {
                        chat.update(cx, |c, cx| c.ask(&p, window, cx));
                    }
                }
                Err(e) => Self::notify_err(window, cx, e),
            })
            .ok();
        })
        .detach();
    }

    /// Launch: reopen the workspace and tab that were open at quit.
    fn restore_last_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((name, tab)) = self.state.last_open.clone() else {
            return;
        };
        let Some(ix) = self.state.workspaces.iter().position(|w| w.name == name) else {
            return;
        };
        self.open_workspace_now(ix, window, cx);
        if tab == "diff" {
            self.show(Tab::Diff, window, cx);
        }
    }

    /// Remembers the open workspace and tab in state.json, for the next launch.
    fn remember_open(&mut self) {
        let tab = match self.tab {
            Tab::Diff => "diff",
            Tab::Chat | Tab::File => "chat",
        };
        let open = self.active.clone().map(|n| (n, tab.to_string()));
        if self.state.last_open != open {
            self.state.last_open = open;
            self.state.save();
        }
    }

    /// "Add repo to workspace…": a menu of the repos the target workspace doesn't have yet.
    fn pick_repo_for_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.target(window) else {
            return Self::notify_err(window, cx, "Open a workspace first");
        };
        let missing: Vec<Repo> = (self.state.repos.iter())
            .filter(|r| !ws.repos.iter().any(|w| w.name == r.name))
            .cloned()
            .collect();
        if missing.is_empty() {
            return Self::notify_err(
                window,
                cx,
                format!("{} has every repo; add another first (space r a)", ws.name),
            );
        }
        let items = missing
            .iter()
            .map(|r| {
                CommandItem::new()
                    .label(r.name.clone())
                    .icon(IconName::FolderGit2)
                    .keywords([r.path.to_string_lossy().to_string()])
            })
            .collect();
        let name = ws.name.clone();
        self.open_menu(
            items,
            0,
            move |s, row, window, cx| {
                s.add_repo_to_workspace(name.clone(), missing[row].clone(), window, cx)
            },
            window,
            cx,
        );
    }

    fn add_repo_to_workspace(
        &mut self,
        name: String,
        repo: Repo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.archiving.contains(&name) {
            return Self::notify_err(window, cx, format!("{name} is being archived"));
        }
        let Some(ws) = self
            .state
            .workspaces
            .iter()
            .find(|w| w.name == name)
            .cloned()
        else {
            return;
        };
        window.push_notification(format!("Adding {} to {name}…", repo.name), cx);
        cx.spawn_in(window, async move |this, cx| {
            let dir = ws.dir();
            let result = cx
                .background_executor()
                .spawn(async move { git::add_to_workspace(&ws, &dir, &repo) })
                .await;
            this.update_in(cx, |s, window, cx| match result {
                Ok(ws) => s.repo_added(ws, window, cx),
                Err(e) => Self::notify_err(window, cx, e),
            })
            .ok();
        })
        .detach();
    }

    /// Saves the new repo list and tells the chat, the git button and the diff.
    fn repo_added(&mut self, ws: Workspace, window: &mut Window, cx: &mut Context<Self>) {
        let Some(slot) = self.state.workspaces.iter_mut().find(|w| w.name == ws.name) else {
            return;
        };
        // Only the repos change; session, model and viewed may have moved on meanwhile.
        // Merge rather than replace: another add may have finished since this one started.
        let Some(r) = ws.repos.last().cloned() else {
            return;
        };
        let raced = slot.repos.len() + 1 != ws.repos.len();
        if !slot.repos.iter().any(|w| w.name == r.name) {
            slot.repos.push(r.clone());
        }
        let ws = slot.clone();
        self.state.save();
        if raced {
            git::write_claude_md(&ws);
        }
        let rules = match ws.dir().join(&r.name).join("CLAUDE.md").exists() {
            true => format!(" Read {}/CLAUDE.md for its rules.", r.name),
            false => String::new(),
        };
        let note = format!(
            "[shark] Repo `{0}` joined this workspace: `{0}/` is a git worktree of {1} on branch `{2}` (base `{3}`), and the root CLAUDE.md now lists it.{rules}",
            r.name,
            r.src.display(),
            ws.branch,
            r.base
        );
        // Every chat of an opened workspace that has (or had) a conversation is told; one never
        // started reads the new root CLAUDE.md at its session start.
        if self.syncs.contains_key(&ws.name) {
            let metas: Vec<ChatMeta> = (ws.chats.iter())
                .filter(|m| self.chats.contains_key(&m.id) || m.session_id.is_some())
                .cloned()
                .collect();
            for m in metas {
                let chat = self.ensure_chat(&ws, &m, window, cx);
                chat.update(cx, |c, cx| c.repos_changed(&ws, note.clone(), cx));
            }
        }
        if let Some(sync) = self.syncs.get(&ws.name) {
            sync.update(cx, |g, cx| g.set_workspace(ws.clone(), cx));
        }
        if self.active.as_ref() == Some(&ws.name) {
            self.refresh_diff(cx);
        }
        window.push_notification(
            Notification::success(format!("Added {} to {}", r.name, ws.name)),
            cx,
        );
        cx.notify();
    }

    /// Row actions hit the workspace under the sidebar cursor when the sidebar is focused, else the open one.
    fn target(&self, window: &Window) -> Option<Workspace> {
        let name = match (self.sidebar_focus.is_focused(window), self.sel) {
            (true, SideRow::Ws(ix)) => self.state.workspaces.get(ix)?.name.clone(),
            _ => self.active.clone()?,
        };
        self.state
            .workspaces
            .iter()
            .find(|w| w.name == name)
            .cloned()
    }

    fn with_target(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&Workspace, &mut Window, &mut App),
    ) {
        match self.target(window) {
            Some(ws) => f(&ws, window, cx),
            None => Self::notify_err(window, cx, "Open a workspace first"),
        }
    }

    fn archive(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ws) = self.target(window) {
            self.archive_ws(ws.name, force, window, cx)
        }
    }

    fn archive_ws(
        &mut self,
        name: String,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.running_chats(&name, cx) > 0 {
            // ctrl-c stops the shown chat's agent only.
            let msg = match self.active.as_deref() == Some(name.as_str()) {
                true => "An agent is running; stop it first (ctrl-c in each running chat)".into(),
                false => {
                    format!("An agent in {name} is running; open it and stop it first (ctrl-c)")
                }
            };
            return Self::notify_err(window, cx, msg);
        }
        if !force {
            return self.remove_workspace(&name, false, window, cx);
        }
        let shark = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |d, _, _| {
            let (shark, name) = (shark.clone(), name.clone());
            d.font_features(inter_features())
                .title(format!("Force-archive {name}?"))
                .description(
                    "Removes its worktrees including uncommitted changes. Branches are kept.",
                )
                .ok_text("Archive")
                .ok_variant(ButtonVariant::Danger)
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    shark
                        .update(cx, |s, cx| s.remove_workspace(&name, true, window, cx))
                        .ok();
                    true
                })
        });
    }

    fn remove_workspace(
        &mut self,
        name: &str,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Git only sees the disk; unsaved editor edits would vanish with the worktree.
        if self.active.as_deref() == Some(name)
            && self.editor.as_ref().is_some_and(|e| e.read(cx).dirty)
        {
            let name = name.to_string();
            return self.confirm_discard(window, cx, move |s, window, cx| {
                s.remove_workspace(&name, force, window, cx)
            });
        }
        let Some(ws) = self
            .state
            .workspaces
            .iter()
            .find(|w| w.name == name)
            .cloned()
        else {
            return;
        };
        // The merge step and the menu can both ask while a removal runs.
        if !self.archiving.insert(ws.name.clone()) {
            return;
        }
        for chat in self.started_chats(name) {
            chat.update(cx, |c, _| c.archiving = true);
        }
        // Force-archive keeps every branch, as its dialog says.
        let merged = match force {
            true => vec![],
            false => self
                .syncs
                .get(name)
                .map_or(vec![], |g| g.read(cx).merged_branches()),
        };
        // `worktree remove` deletes node_modules, target and the like: seconds, off the UI thread.
        window.push_notification(format!("Archiving {name}…"), cx);
        cx.spawn_in(window, async move |this, cx| {
            let dir = ws.dir();
            let name = ws.name.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    git::remove_workspace(&ws, &dir, force)
                        .map(|()| git::delete_merged_branches(&ws, &merged))
                })
                .await;
            this.update_in(cx, |s, window, cx| {
                s.archiving.remove(&name);
                s.archived(&name, result, window, cx)
            })
            .ok();
        })
        .detach();
    }

    fn archived(
        &mut self,
        name: &str,
        result: Result<Vec<String>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(deleted) => {
                let pos = self.side_rows().iter().position(|r| *r == self.sel);
                // A later workspace with the same name must not replay or resume these chats.
                if let Some(ws) = self.state.workspaces.iter().find(|w| w.name == name) {
                    for c in ws.chats.iter().chain(&ws.closed) {
                        self.chats.remove(&c.id);
                        self.unread.remove(&c.id);
                        let _ = std::fs::remove_file(crate::agent::transcript_path(&c.id));
                    }
                }
                self.syncs.remove(name);
                self.state.workspaces.retain(|w| w.name != name);
                for s in &mut self.state.sections {
                    s.workspaces.retain(|w| w != name);
                }
                self.state.save();
                if self.active.as_deref() == Some(name) {
                    self.active = None;
                    self.drop_editor();
                    self.tab = Tab::Chat;
                    self.sidebar_focus.focus(window, cx);
                }
                // The cursor stays at the same height, on whatever row moved up into it.
                let rows = self.side_rows();
                self.sel = pos
                    .and_then(|p| rows.get(p.min(rows.len().saturating_sub(1))))
                    .copied()
                    .unwrap_or(SideRow::Ws(0));
                let branch = match deleted.is_empty() {
                    true => "branch kept".to_string(),
                    false => format!("merged branch deleted in {}", deleted.join(", ")),
                };
                window.push_notification(
                    Notification::success(format!("Archived {name} ({branch})")),
                    cx,
                );
            }
            Err(e) => {
                for chat in self.started_chats(name) {
                    chat.update(cx, |c, _| c.archiving = false);
                }
                Self::notify_err(window, cx, format!("{e}\nUse space w D to force."))
            }
        }
        cx.notify();
    }

    fn create_prs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.with_sync(window, cx, |g, window, cx| {
            g.run_kind(
                sync::Kind::CreatePr,
                "Create / update linked PRs",
                window,
                cx,
            )
        })
    }

    fn create_draft_prs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.with_sync(window, cx, |g, window, cx| {
            g.run_kind(sync::Kind::CreateDraftPr, "Create draft PRs", window, cx)
        })
    }

    fn open_prs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.with_sync(window, cx, GitSync::open_prs)
    }

    fn focus_queue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(chat) = self.active_chat().cloned() {
            self.show(Tab::Chat, window, cx);
            chat.update(cx, |c, cx| c.focus_queue(window, cx));
        }
    }

    /// How many of a workspace's chats have something going on.
    pub(crate) fn running_chats(&self, ws: &str, cx: &App) -> usize {
        (self.started_chats(ws).iter())
            .filter(|c| c.read(cx).working())
            .count()
    }

    /// A workspace's open chat tabs that have been shown (and so have an entity).
    fn started_chats(&self, ws: &str) -> Vec<Entity<Chat>> {
        let Some(ws) = self.state.workspaces.iter().find(|w| w.name == ws) else {
            return vec![];
        };
        (ws.chats.iter())
            .filter_map(|c| self.chats.get(&c.id).cloned())
            .collect()
    }

    /// A chat tab of the workspace finished a turn that hasn't been looked at.
    pub(crate) fn chat_unread(&self, ws: &str) -> bool {
        (self.state.workspaces.iter())
            .find(|w| w.name == ws)
            .is_some_and(|w| w.chats.iter().any(|c| self.unread.contains(&c.id)))
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(chat) = self.active_chat() {
            chat.update(cx, |c, cx| c.stop(cx));
        }
    }

    fn open_menu(
        &self,
        items: Vec<CommandItem>,
        selected: usize,
        on_pick: impl Fn(&mut Shark, usize, &mut Window, &mut Context<Shark>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        open_menu(
            cx.entity().downgrade(),
            items,
            selected,
            on_pick,
            window,
            cx,
        )
    }

    fn pick_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = self.active_chat().cloned() else {
            return Self::notify_err(window, cx, "Open a workspace first");
        };
        let current = chat.read(cx).model().to_string();
        let items = MODELS
            .iter()
            .map(|m| {
                CommandItem::new()
                    .label(*m)
                    .icon(IconName::Sparkles)
                    .checked(*m == current)
            })
            .collect();
        let selected = MODELS.iter().position(|m| *m == current).unwrap_or(0);
        self.open_menu(
            items,
            selected,
            move |_, row, _, cx| chat.update(cx, |c, cx| c.set_model(MODELS[row], cx)),
            window,
            cx,
        );
    }

    /// The no-menu fast path, so confirm the switch in a toast.
    fn set_model(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(chat), Some(m)) = (self.active_chat().cloned(), MODELS.get(ix)) else {
            return Self::notify_err(window, cx, "Open a workspace first");
        };
        chat.update(cx, |c, cx| c.set_model(m, cx));
        window.push_notification(format!("Model: {m} (from the next message)"), cx);
    }

    fn section_of(&self, ws: &str) -> Option<usize> {
        self.state
            .sections
            .iter()
            .position(|s| s.workspaces.iter().any(|w| w == ws))
    }

    /// Sidebar rows top to bottom: each section, then its workspaces unless it is
    /// collapsed; then the workspaces in no section.
    fn side_rows(&self) -> Vec<SideRow> {
        let ws = &self.state.workspaces;
        let mut rows = vec![];
        for (i, s) in self.state.sections.iter().enumerate() {
            rows.push(SideRow::Section(i));
            if !s.collapsed {
                rows.extend(
                    s.workspaces
                        .iter()
                        .filter_map(|n| ws.iter().position(|w| &w.name == n))
                        .map(SideRow::Ws),
                );
            }
        }
        rows.extend(
            (0..ws.len())
                .filter(|&ix| self.section_of(&ws[ix].name).is_none())
                .map(SideRow::Ws),
        );
        rows
    }

    /// Every workspace in sidebar order, folded sections included (`cmd-j` walks this).
    pub(crate) fn sidebar_order(&self) -> Vec<usize> {
        let ws = &self.state.workspaces;
        let pos = |n: &String| ws.iter().position(|w| &w.name == n);
        let mut order: Vec<usize> = self
            .state
            .sections
            .iter()
            .flat_map(|s| s.workspaces.iter().filter_map(pos))
            .collect();
        order.extend((0..ws.len()).filter(|&ix| self.section_of(&ws[ix].name).is_none()));
        order
    }

    /// Visible workspaces in sidebar order; cmd-N opens the Nth.
    fn ws_order(&self) -> Vec<usize> {
        self.side_rows()
            .into_iter()
            .filter_map(|r| match r {
                SideRow::Ws(ix) => Some(ix),
                SideRow::Section(_) => None,
            })
            .collect()
    }

    /// Keeps the cursor on a visible row; a workspace hidden by collapsing hands it to its header.
    fn fix_sel(&mut self) {
        let rows = self.side_rows();
        if rows.contains(&self.sel) {
            return;
        }
        let header = match self.sel {
            SideRow::Ws(ix) => self
                .state
                .workspaces
                .get(ix)
                .and_then(|w| self.section_of(&w.name)),
            SideRow::Section(_) => None,
        };
        self.sel = header
            .map(SideRow::Section)
            .or(rows.first().copied())
            .unwrap_or(SideRow::Ws(0));
    }

    fn move_sel(&mut self, to: impl FnOnce(usize, usize) -> usize, cx: &mut Context<Self>) {
        let rows = self.side_rows();
        if rows.is_empty() {
            return;
        }
        let at = rows.iter().position(|r| *r == self.sel).unwrap_or(0);
        let to = to(at, rows.len()).min(rows.len() - 1);
        self.sel = rows[to];
        // Child 0 of the scrolled list is the Workspaces header.
        self.sidebar_scroll.scroll_to_item(to + 1);
        cx.notify();
    }

    /// Section actions hit the section under the sidebar cursor (or the cursor
    /// workspace's section) when the sidebar is focused, else the open workspace's.
    fn target_section(&self, window: &Window) -> Option<usize> {
        if self.sidebar_focus.is_focused(window) {
            return match self.sel {
                SideRow::Section(i) => Some(i),
                SideRow::Ws(ix) => self.section_of(&self.state.workspaces.get(ix)?.name),
            };
        }
        self.section_of(self.active.as_ref()?)
    }

    fn with_section(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Self, usize, &mut Window, &mut Context<Self>),
    ) {
        match self.target_section(window) {
            Some(i) => f(self, i, window, cx),
            None => Self::notify_err(window, cx, "Put the sidebar cursor on a section first"),
        }
    }

    fn set_collapsed(&mut self, i: usize, collapsed: bool, cx: &mut Context<Self>) {
        if let Some(s) = self.state.sections.get_mut(i)
            && s.collapsed != collapsed
        {
            s.collapsed = collapsed;
            self.state.save();
            self.fix_sel();
            cx.notify();
        }
    }

    fn toggle_section(&mut self, i: usize, cx: &mut Context<Self>) {
        if let Some(s) = self.state.sections.get(i) {
            self.set_collapsed(i, !s.collapsed, cx);
        }
    }

    /// `h` in the sidebar: fold the cursor's section and park the cursor on its header.
    fn collapse_at_cursor(&mut self, cx: &mut Context<Self>) {
        let i = match self.sel {
            SideRow::Section(i) => Some(i),
            SideRow::Ws(ix) => self
                .state
                .workspaces
                .get(ix)
                .and_then(|w| self.section_of(&w.name)),
        };
        if let Some(i) = i {
            self.sel = SideRow::Section(i);
            self.set_collapsed(i, true, cx);
            cx.notify();
        }
    }

    fn move_to_section(&mut self, ws: &str, to: Option<usize>, cx: &mut Context<Self>) {
        if self.section_of(ws) == to {
            return;
        }
        for s in &mut self.state.sections {
            s.workspaces.retain(|w| w != ws);
        }
        if let Some(s) = to.and_then(|i| self.state.sections.get_mut(i)) {
            s.workspaces.push(ws.to_string());
        }
        self.state.save();
        self.fix_sel();
        cx.notify();
    }

    /// The workspace under the cursor, so a reorder that shifts indices can put it back.
    fn sel_name(&self) -> Option<String> {
        match self.sel {
            SideRow::Ws(ix) => Some(self.state.workspaces.get(ix)?.name.clone()),
            SideRow::Section(_) => None,
        }
    }

    fn drop_ws(&mut self, ws: &str, target: &str, cx: &mut Context<Self>) {
        let cursor = self.sel_name();
        self.state.drop_ws(ws, target);
        if let Some(ix) =
            cursor.and_then(|n| self.state.workspaces.iter().position(|w| w.name == n))
        {
            self.sel = SideRow::Ws(ix);
        }
        self.state.save();
        self.fix_sel();
        cx.notify();
    }

    fn drop_section(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let n = self.state.sections.len();
        if from == to || from >= n || to >= n {
            return;
        }
        let cursor = match self.sel {
            SideRow::Section(j) => Some(self.state.sections[j].name.clone()),
            SideRow::Ws(_) => None,
        };
        move_item(&mut self.state.sections, from, to);
        if let Some(j) = cursor.and_then(|c| self.state.sections.iter().position(|s| s.name == c)) {
            self.sel = SideRow::Section(j);
        }
        if self.naming.as_ref().is_some_and(|n| n.rename.is_some()) {
            self.naming = None; // its index moved
        }
        self.state.save();
        cx.notify();
    }

    fn delete_section(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i >= self.state.sections.len() {
            return;
        }
        let s = self.state.sections.remove(i);
        self.state.save();
        self.naming = None;
        if let SideRow::Section(j) = self.sel {
            self.sel = SideRow::Section(if j > i { j - 1 } else { j });
        }
        self.fix_sel();
        window.push_notification(
            format!("Deleted section {} (its workspaces stay)", s.name),
            cx,
        );
        cx.notify();
    }

    /// Opens the inline name field: on section `rename`, or below the sections for a new one.
    fn start_naming(
        &mut self,
        rename: Option<usize>,
        then_move: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value = rename
            .and_then(|i| self.state.sections.get(i))
            .map(|s| s.name.clone())
            .unwrap_or_default();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Section name")
                .default_value(value)
        });
        let _sub = cx.subscribe_in(
            &input,
            window,
            |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter { .. } => this.commit_naming(true, window, cx),
                InputEvent::Blur => this.commit_naming(false, window, cx),
                _ => {}
            },
        );
        // Deferred: a closing menu or palette restores focus first, which would blur the field.
        let field = input.clone();
        window.defer(cx, move |window, cx| {
            field.update(cx, |i, cx| {
                i.focus(window, cx);
                i.select_all(window, cx);
            })
        });
        self.rail = false;
        self.naming = Some(Naming {
            rename,
            then_move,
            input,
            _sub,
        });
        cx.notify();
    }

    /// Enter saves (a taken name stays in the field); clicking away saves too.
    /// An empty name changes nothing.
    fn commit_naming(&mut self, enter: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(n) = &self.naming else {
            return;
        };
        let name = n.input.read(cx).value().trim().to_string();
        let taken = self
            .state
            .sections
            .iter()
            .enumerate()
            .any(|(i, s)| s.name == name && Some(i) != n.rename);
        if taken && enter {
            return Self::notify_err(window, cx, format!("There is already a section {name}"));
        }
        let Some(n) = self.naming.take() else {
            return;
        };
        if !name.is_empty() && !taken {
            match n.rename {
                Some(i) => {
                    if let Some(s) = self.state.sections.get_mut(i) {
                        s.name = name;
                    }
                }
                None => {
                    self.state.sections.push(Section {
                        name,
                        icon: "folder".into(),
                        collapsed: false,
                        workspaces: vec![],
                    });
                    let i = self.state.sections.len() - 1;
                    self.sel = SideRow::Section(i);
                    if let Some(ws) = n.then_move {
                        self.move_to_section(&ws, Some(i), cx);
                    }
                }
            }
            self.state.save();
        }
        if enter {
            self.sidebar_focus.focus(window, cx);
        }
        cx.notify();
    }

    fn cancel_naming(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.naming = None;
        self.sidebar_focus.focus(window, cx);
        cx.notify();
    }

    fn pick_icon(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(current) = self.state.sections.get(i).map(|s| s.icon.clone()) else {
            return;
        };
        // Named by keyword, so typing "bug" or "star" finds the icon.
        let items = SECTION_ICONS
            .iter()
            .map(|(name, icon)| {
                CommandItem::new()
                    .label(name.replace('-', " "))
                    .icon(*icon)
                    .checked(*name == current)
            })
            .collect();
        let selected = SECTION_ICONS
            .iter()
            .position(|(n, _)| *n == current)
            .unwrap_or(0);
        self.open_menu(
            items,
            selected,
            move |s, row, _, cx| {
                if let Some(sec) = s.state.sections.get_mut(i) {
                    sec.icon = SECTION_ICONS[row].0.into();
                    s.state.save();
                    cx.notify();
                }
            },
            window,
            cx,
        );
    }

    /// Sections, then "No section" when it is in one, then "New section…".
    fn pick_section(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.target(window).map(|w| w.name) else {
            return Self::notify_err(window, cx, "Open a workspace first");
        };
        let current = self.section_of(&ws);
        let n = self.state.sections.len();
        let mut items: Vec<CommandItem> = self
            .state
            .sections
            .iter()
            .enumerate()
            .map(|(i, s)| {
                CommandItem::new()
                    .label(s.name.clone())
                    .icon(section_icon(&s.icon))
                    .checked(current == Some(i))
            })
            .collect();
        if current.is_some() {
            items.push(
                CommandItem::new()
                    .label("No section")
                    .icon(IconName::FolderOutput),
            );
        }
        items.push(
            CommandItem::new()
                .label("New section…")
                .icon(IconName::FolderPlus),
        );
        let last = items.len() - 1;
        self.open_menu(
            items,
            current.unwrap_or(0),
            move |s, row, window, cx| match row {
                _ if row == last => s.start_naming(None, Some(ws.clone()), window, cx),
                _ if row < n => s.move_to_section(&ws, Some(row), cx),
                _ => s.move_to_section(&ws, None, cx),
            },
            window,
            cx,
        );
    }

    fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workspaces: Vec<String> = self
            .state
            .workspaces
            .iter()
            .map(|w| w.name.clone())
            .collect();
        let items: Vec<CommandItem> = COMMANDS
            .iter()
            .map(|(label, icon, _)| CommandItem::new().label(*label).icon(*icon))
            .chain(workspaces.iter().map(|w| {
                CommandItem::new()
                    .label(format!("Open workspace: {w}"))
                    .icon(IconName::GitBranch)
            }))
            .collect();
        self.open_menu(
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

    fn render_sidebar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let focused = self.sidebar_focus.is_focused(window);
        let rail = self.rail;
        let header = |t: &'static str| {
            div()
                .when(rail, |d| d.invisible())
                .px_4()
                .pt_4()
                .pb_1()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.muted_foreground)
                .child(t)
        };
        let hint = |t: &'static str| {
            div()
                .when(rail, |d| d.hidden())
                .px_4()
                .text_xs()
                .text_color(faint(theme))
                .child(t)
        };
        let row = || {
            h_flex()
                .h(px(28.))
                .flex_none()
                .mx_2()
                .mb_1() // so a hovered row never touches the selected one
                .px_2()
                .gap_2()
                .rounded(theme.radius)
                .text_sm()
                .when(rail, |d| d.justify_center())
        };
        let m = window.modifiers();
        let cmd_held = m.platform && m.number_of_modifiers() == 1;
        let shark = cx.entity().downgrade();
        let repos = self.state.repos.iter().enumerate().map(|(ix, r)| {
            let (name, path) = (r.name.clone(), r.path.clone());
            let tip: SharedString = name.clone().into();
            row()
                .id(("repo", ix))
                .text_color(theme.muted_foreground)
                .hover(|d| d.bg(theme.sidebar_accent))
                .child(Icon::new(IconName::FolderGit2).with_size(px(14.)))
                .when(!rail, |d| d.child(div().truncate().child(r.name.clone())))
                .when(rail, |d| {
                    d.tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
                })
                .context_menu(move |menu, _, _| {
                    let (p1, p2, p3) = (path.clone(), path.clone(), path.clone());
                    menu.label(format!("{name} — {}", tilde(&path)))
                        .separator()
                        .item(
                            PopupMenuItem::new("Reveal in Finder")
                                .icon(IconName::FolderOpen)
                                .on_click(move |_, _, cx| cx.reveal_path(&p1)),
                        )
                        .item(
                            PopupMenuItem::new("Open in Terminal")
                                .icon(IconName::SquareTerminal)
                                .on_click(move |_, _, _| open_terminal(&p2)),
                        )
                        .item(
                            PopupMenuItem::new("Copy Path")
                                .icon(IconName::Copy)
                                .on_click(move |_, w, cx| copy(p3.display().to_string(), w, cx)),
                        )
                })
        });
        let order = self.ws_order();
        let (primary, accent) = (theme.primary, theme.sidebar_accent);
        // Dropping a workspace on a header or on a row of a section moves it there.
        let drop_on = |to: Option<usize>| {
            cx.listener(move |this: &mut Shark, d: &DraggedWs, _, cx| {
                this.move_to_section(&d.0, to, cx)
            })
        };
        let ws_row = |ix: usize, w: &Workspace, section: Option<usize>| {
            let running = self.running_chats(&w.name, cx);
            let busy = running > 0;
            let selected = self.sel == SideRow::Ws(ix);
            let active = self.active.as_ref() == Some(&w.name);
            let attn = self.attention_of(&w.name, cx);
            let nth = order.iter().position(|&o| o == ix).filter(|&n| n < 9);
            let ws = w.clone();
            let shark = shark.clone();
            row()
                .id(ix)
                .h(px(44.))
                .when(section.is_some() && !rail, |d| d.ml(px(24.)))
                .border_1()
                .border_color(theme.transparent)
                // The tint alone is too subtle to find the cursor; the accent ring makes it visible.
                .when(selected && focused, |d| {
                    d.bg(theme.list_active).border_color(theme.primary)
                })
                .when(selected && !focused, |d| d.bg(theme.sidebar_accent))
                .hover(|d| d.bg(theme.sidebar_accent))
                .on_click(
                    cx.listener(move |this, _, window, cx| this.open_workspace(ix, window, cx)),
                )
                // Right-click moves the cursor too, so it's clear which row the menu is for.
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _, _, cx| {
                        this.sel = SideRow::Ws(ix);
                        cx.notify();
                    }),
                )
                .on_drag(DraggedWs(w.name.clone().into()), |d, _, _, cx| {
                    cx.new(|_| d.clone())
                })
                .drag_over::<DraggedWs>(move |d, _, _, _| d.border_color(primary))
                .on_drop({
                    let target = w.name.clone();
                    cx.listener(move |this, d: &DraggedWs, _, cx| this.drop_ws(&d.0, &target, cx))
                })
                .child(match (busy, nth) {
                    (_, Some(n)) if cmd_held => {
                        Kbd::new(Keystroke::parse(&format!("cmd-{}", n + 1)).unwrap())
                            .into_any_element()
                    }
                    (true, _) => Spinner::new()
                        .icon(IconName::LoaderCircle)
                        .color(theme.primary)
                        .with_size(px(14.))
                        .into_any_element(),
                    (false, _) => {
                        let (icon, color) = match &attn {
                            Attn::Needs(_) => (IconName::CircleAlert, theme.warning_foreground),
                            Attn::Unread => (IconName::CircleCheck, theme.success_foreground),
                            Attn::None => (IconName::CircleDashed, theme.muted_foreground),
                        };
                        Icon::new(icon)
                            .with_size(px(14.))
                            .text_color(color)
                            .into_any_element()
                    }
                })
                .when(!rail, |d| {
                    d.child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .when(active, |d| d.font_weight(FontWeight::MEDIUM))
                                    .child(w.name.clone()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child({
                                        let repos = (w.repos.iter())
                                            .map(|r| r.name.as_str())
                                            .collect::<Vec<_>>()
                                            .join(" · ");
                                        match (&attn, running) {
                                            // The step says where, and that it's waiting on you.
                                            (Attn::Needs(step), _) => step.clone(),
                                            (_, 0 | 1) => repos,
                                            (_, n) => format!("{n} chats running · {repos}"),
                                        }
                                    }),
                            ),
                    )
                })
                .when(rail, |d| {
                    let name: SharedString = w.name.clone().into();
                    d.tooltip(move |w, cx| Tooltip::new(name.clone()).build(w, cx))
                })
                .context_menu(move |menu, window, cx| ws_menu(menu, &ws, ix, &shark, window, cx))
                .into_any_element()
        };
        let section_row =
            |i: usize, sec: &Section| {
                let selected = self.sel == SideRow::Section(i);
                let (name, shark) = (sec.name.clone(), shark.clone());
                let tip: SharedString = sec.name.clone().into();
                // A folded section still shows that a workspace inside needs you.
                let needs = sec.collapsed
                    && sec
                        .workspaces
                        .iter()
                        .any(|n| matches!(self.attention_of(n, cx), Attn::Needs(_)));
                row()
                    .id(("section", i))
                    .border_1()
                    .border_color(theme.transparent)
                    .text_color(theme.muted_foreground)
                    .when(selected && focused, |d| {
                        d.bg(theme.list_active).border_color(theme.primary)
                    })
                    .when(selected && !focused, |d| d.bg(theme.sidebar_accent))
                    .hover(|d| d.bg(theme.sidebar_accent))
                    .drag_over::<DraggedWs>(move |d, _, _, _| d.bg(accent).border_color(primary))
                    .on_drop(drop_on(Some(i)))
                    .on_drag(DraggedSection(i, sec.name.clone().into()), |d, _, _, cx| {
                        cx.new(|_| d.clone())
                    })
                    .drag_over::<DraggedSection>(move |d, _, _, _| d.border_color(primary))
                    .on_drop(cx.listener(move |this, d: &DraggedSection, _, cx| {
                        this.drop_section(d.0, i, cx)
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.sel = SideRow::Section(i);
                        this.toggle_section(i, cx)
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, _, _, cx| {
                            this.sel = SideRow::Section(i);
                            cx.notify();
                        }),
                    )
                    .child(
                        Icon::new(section_icon(&sec.icon))
                            .with_size(px(14.))
                            .when(needs, |i| i.text_color(theme.warning_foreground)),
                    )
                    .when(!rail, |d| {
                        d.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_weight(FontWeight::MEDIUM)
                                .child(sec.name.clone()),
                        )
                        .when(sec.collapsed, |d| {
                            d.child(div().text_xs().child(sec.workspaces.len().to_string()))
                        })
                        .child(
                            Icon::new(match sec.collapsed {
                                true => IconName::ChevronRight,
                                false => IconName::ChevronDown,
                            })
                            .with_size(px(12.)),
                        )
                    })
                    .when(rail, |d| {
                        d.tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
                    })
                    .context_menu(move |menu, _, _| section_menu(menu, i, &name, &shark))
                    .into_any_element()
            };
        let name_row = |input: &Entity<InputState>, icon: IconName| {
            row()
                .key_context("SectionName")
                .border_1()
                .border_color(theme.primary)
                .bg(theme.list_active)
                .text_color(theme.muted_foreground)
                .on_action(cx.listener(|this, _: &CancelEdit, w, cx| this.cancel_naming(w, cx)))
                .child(Icon::new(icon).with_size(px(14.)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(theme.foreground)
                        .child(Input::new(input).appearance(false).small()),
                )
                .into_any_element()
        };
        let mut items: Vec<AnyElement> = vec![];
        let mut new_row = self
            .naming
            .as_ref()
            .filter(|n| n.rename.is_none())
            .map(|n| name_row(&n.input, IconName::Folder));
        for r in self.side_rows() {
            match r {
                SideRow::Section(i) => {
                    let sec = &self.state.sections[i];
                    match &self.naming {
                        Some(n) if n.rename == Some(i) => {
                            items.push(name_row(&n.input, section_icon(&sec.icon)))
                        }
                        _ => items.push(section_row(i, sec)),
                    }
                }
                SideRow::Ws(ix) => {
                    let w = &self.state.workspaces[ix];
                    let section = self.section_of(&w.name);
                    if section.is_none() {
                        // A new section's field goes below the existing sections.
                        items.extend(new_row.take());
                    }
                    items.push(ws_row(ix, w, section));
                }
            }
        }
        items.extend(new_row.take());
        let new_section_menu = {
            let shark = shark.clone();
            move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
                let shark = shark.clone();
                menu.item(
                    PopupMenuItem::new("New Section")
                        .icon(IconName::FolderPlus)
                        .on_click(move |_, window, cx| {
                            shark
                                .update(cx, |s, cx| s.start_naming(None, None, window, cx))
                                .ok();
                        }),
                )
            }
        };
        let ws_header = h_flex()
            .id("ws-header")
            .group("ws-header")
            .justify_between()
            .items_end()
            .drag_over::<DraggedWs>(move |d, _, _, _| d.bg(accent))
            .on_drop(drop_on(None))
            .child(header("Workspaces"))
            .when(!rail, |d| {
                d.child(
                    div()
                        .mr_3()
                        .invisible()
                        .group_hover("ws-header", |s| s.visible())
                        .child(
                            Button::new("new-section")
                                .ghost()
                                .xsmall()
                                .icon(IconName::FolderPlus)
                                .tooltip("New section (space s n)")
                                .on_click(cx.listener(|this, _, w, cx| {
                                    this.start_naming(None, None, w, cx)
                                })),
                        ),
                )
            })
            .context_menu(new_section_menu.clone());
        v_flex()
            .id("sidebar")
            .key_context("Sidebar")
            .track_focus(&self.sidebar_focus)
            .w(px(if rail { 48. } else { 244. }))
            .h_full()
            .flex_none()
            .text_color(theme.sidebar_foreground)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_sel(|at, _| at + 1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| {
                this.move_sel(|at, _| at.saturating_sub(1), cx)
            }))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| this.move_sel(|_, _| 0, cx)))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| this.move_sel(|_, n| n - 1, cx)))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| match this.sel {
                SideRow::Ws(ix) => this.open_workspace(ix, window, cx),
                SideRow::Section(i) => this.toggle_section(i, cx),
            }))
            .on_action(cx.listener(|this, _: &CollapseDir, _, cx| this.collapse_at_cursor(cx)))
            .on_action(cx.listener(|this, _: &ExpandDir, _, cx| {
                if let SideRow::Section(i) = this.sel {
                    this.set_collapsed(i, false, cx)
                }
            }))
            // Room for the traffic lights; the transparent titlebar lets the sidebar run under them.
            .child(
                div()
                    .h(px(36.))
                    .flex_none()
                    .window_control_area(WindowControlArea::Drag),
            )
            .child(
                // Children are indexed by `move_sel`: the header, then one per `side_rows` row.
                v_flex()
                    .id("sidebar-rows")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.sidebar_scroll)
                    .child(ws_header)
                    .children(items)
                    .when(
                        self.state.workspaces.is_empty() && self.state.sections.is_empty(),
                        |d| d.child(hint("space w n to create")),
                    )
                    .child(header("Repos"))
                    .children(repos)
                    .when(self.state.repos.is_empty(), |d| {
                        d.child(hint("space r a to add"))
                    })
                    // The empty space below takes a right-click too.
                    .child(
                        div()
                            .id("sidebar-rest")
                            .flex_1()
                            .min_h(px(24.))
                            .context_menu(new_section_menu),
                    ),
            )
    }

    fn render_main(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let chat_tabs = self.render_chat_tabs(cx);
        let theme = cx.theme();
        let Some(chat) = self.active_chat().cloned() else {
            let key = |k: &str| Kbd::new(Keystroke::parse(k).unwrap());
            let tip = |keys: Vec<Kbd>, label: &'static str| {
                h_flex()
                    .gap_2()
                    .child(h_flex().gap_1().children(keys))
                    .child(label)
            };
            return v_flex()
                .flex_1()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .text_color(theme.muted_foreground)
                .child(
                    Icon::new(IconName::Fish)
                        .with_size(px(32.))
                        .text_color(faint(theme)),
                )
                .child(
                    div()
                        .text_base()
                        .text_color(theme.foreground)
                        .child("No workspace open"),
                )
                .child(
                    v_flex()
                        .gap_1p5()
                        .text_sm()
                        .child(tip(
                            vec![key("j"), key("k"), key("enter")],
                            "open from sidebar",
                        ))
                        .child(tip(vec![key("space"), key("w"), key("n")], "new workspace"))
                        .child(tip(vec![key("space"), key("r"), key("a")], "add repo"))
                        .child(tip(vec![key("cmd-k")], "commands")),
                )
                .into_any_element();
        };
        let tab = |label: String, icon: IconName, t: Tab| {
            let on = self.tab == t;
            h_flex()
                .id(label.clone())
                .h(px(26.))
                .px_2p5()
                .gap_1p5()
                .rounded_full()
                .text_sm()
                .text_color(if on {
                    theme.foreground
                } else {
                    theme.muted_foreground
                })
                .when(on, |d| d.bg(theme.secondary))
                .hover(|d| d.text_color(theme.foreground))
                .on_click(cx.listener(move |this, _, window, cx| this.show(t, window, cx)))
                .child(Icon::new(icon).with_size(px(14.)))
                .child(label)
        };
        let viewed = self.viewed_now.len();
        let file_tab = self.editor.as_ref().map(|e| {
            let path = &e.read(cx).path;
            path.rsplit('/').next().unwrap_or(path).to_string()
        });
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                h_flex()
                    .h(px(44.))
                    .flex_none()
                    .px_3()
                    // The rail is narrower than the traffic lights; keep the header clear of them.
                    .when(self.rail, |d| d.pl(px(36.)))
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .window_control_area(WindowControlArea::Drag)
                    .child(
                        Button::new("toggle-sidebar")
                            .ghost()
                            .xsmall()
                            .icon(if self.rail {
                                IconName::PanelLeftOpen
                            } else {
                                IconName::PanelLeftClose
                            })
                            .tooltip("Toggle sidebar (cmd-b)")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
                    )
                    .children(chat_tabs)
                    .child(tab(
                        match viewed {
                            _ if self.diff_error.is_some() => "Diff · error".into(),
                            _ if !self.diff_loaded => "Diff".into(),
                            0 => format!("Diff · {}", self.diff_files.len()),
                            v => format!("Diff · {v}/{} viewed", self.diff_files.len()),
                        },
                        IconName::FileDiff,
                        Tab::Diff,
                    ))
                    .when_some(file_tab, |d, name| {
                        d.child(tab(name, IconName::FileCode, Tab::File))
                    })
                    .child(div().flex_1())
                    .child(self.render_branch(cx))
                    .when_some(self.active_sync().cloned(), |d, sync| {
                        d.child(div().pl_3().child(sync))
                    })
                    .child(self.render_status(&chat, cx)),
            )
            .child(match self.tab {
                // Cached: the sidebar spinner redraws this view every frame, the chat only on change.
                Tab::Chat => div()
                    .flex_1()
                    .min_h_0()
                    .child(chat.cached(StyleRefinement::default().size_full()))
                    .into_any_element(),
                Tab::File => h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        h_resizable("chat-editor")
                            .child(
                                resizable_panel()
                                    .size(px(420.))
                                    .size_range(px(320.)..px(900.))
                                    .child(chat.cached(StyleRefinement::default().size_full())),
                            )
                            .child(resizable_panel().child(self.render_editor(cx))),
                    )
                    .when(self.show_tree, |d| d.child(self.render_tree(window, cx)))
                    .into_any_element(),
                Tab::Diff => h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_diff(cx))
                    .when(self.show_tree, |d| d.child(self.render_tree(window, cx)))
                    .into_any_element(),
            })
            .into_any_element()
    }

    /// Loading, git failed, or nothing changed; `None` when there is a diff to show.
    fn diff_placeholder(&self, cx: &App) -> Option<AnyElement> {
        let theme = cx.theme();
        let big = |name, color| Icon::new(name).with_size(px(32.)).text_color(color);
        let (icon, title, detail) = match &self.diff_error {
            Some(e) => (
                big(IconName::TriangleAlert, theme.warning_foreground).into_any_element(),
                "Could not load the diff",
                Some(e.clone()),
            ),
            None if !self.diff_loaded => (
                Spinner::new()
                    .with_size(px(32.))
                    .color(faint(theme))
                    .into_any_element(),
                "Loading diff…",
                None,
            ),
            None if self.diff_files.is_empty() => (
                big(IconName::FileDiff, faint(theme)).into_any_element(),
                "No changes",
                None,
            ),
            None => return None,
        };
        Some(
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .px_4()
                .text_color(theme.muted_foreground)
                .child(icon)
                .child(div().text_base().text_color(theme.foreground).child(title))
                .when_some(detail, |d, e| {
                    d.child(div().max_w(px(560.)).text_sm().text_center().child(e))
                })
                .child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .child(Kbd::new(Keystroke::parse("r").unwrap()))
                        .child("refresh"),
                )
                .into_any_element(),
        )
    }

    /// One pill per chat, like the Diff and file tabs; a single chat reads "Chat" as before.
    fn render_chat_tabs(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(ws) = self.active_ws() else {
            return vec![];
        };
        let theme = cx.theme();
        let many = ws.chats.len() > 1;
        let mut tabs: Vec<AnyElement> = (ws.chats.iter().enumerate())
            .map(|(ix, meta)| {
                let chat = self.chats.get(&meta.id).map(|c| c.read(cx));
                let on = ix == ws.active_chat && self.tab == Tab::Chat;
                let current = ix == ws.active_chat;
                let label: SharedString = match (many, chat) {
                    (false, _) => "Chat".into(),
                    (true, Some(c)) => c.title(),
                    (true, None) if meta.title.is_empty() => "New chat".into(),
                    (true, None) => meta.title.clone().into(),
                };
                let icon = match (
                    chat.is_some_and(|c| c.working()),
                    self.unread.contains(&meta.id),
                ) {
                    (true, _) => Spinner::new()
                        .icon(IconName::LoaderCircle)
                        .color(theme.primary)
                        .with_size(px(14.))
                        .into_any_element(),
                    (false, unread) => div()
                        .relative()
                        .child(Icon::new(IconName::MessageSquare).with_size(px(14.)))
                        // A finished turn you haven't looked at yet.
                        .when(unread, |d| {
                            d.child(
                                div()
                                    .absolute()
                                    .top(px(-2.))
                                    .right(px(-2.))
                                    .size(px(6.))
                                    .rounded_full()
                                    .bg(theme.primary),
                            )
                        })
                        .into_any_element(),
                };
                h_flex()
                    .id(SharedString::from(format!("chat-tab-{}", meta.id)))
                    .h(px(26.))
                    .pl_2p5()
                    .when(!(many && current), |d| d.pr_2p5())
                    .when(many && current, |d| d.pr_1())
                    .gap_1p5()
                    .rounded_full()
                    .text_sm()
                    .text_color(if on {
                        theme.foreground
                    } else {
                        theme.muted_foreground
                    })
                    .when(on, |d| d.bg(theme.secondary))
                    .hover(|d| d.text_color(theme.foreground))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_chat(ix, window, cx);
                        this.show(Tab::Chat, window, cx);
                    }))
                    .child(icon)
                    .child(div().max_w(px(180.)).truncate().child(label))
                    .when(many && current, |d| {
                        d.child(
                            Button::new("close-chat")
                                .ghost()
                                .xsmall()
                                .icon(IconName::X)
                                .tooltip("Close chat (cmd-w)")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.close_chat(window, cx)
                                })),
                        )
                    })
                    .into_any_element()
            })
            .collect();
        tabs.push(
            Button::new("new-chat")
                .ghost()
                .xsmall()
                .icon(IconName::Plus)
                .tooltip("New chat (cmd-t)")
                .on_click(cx.listener(|this, _, window, cx| this.new_chat(window, cx)))
                .into_any_element(),
        );
        tabs
    }

    /// The header's agent status, naming the call in flight; hovering shows the turn's details
    /// and what runs in the background.
    fn render_status(&self, chat: &Entity<Chat>, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let c = chat.read(cx);
        let running = match c.running_tool() {
            Some(t) => format!("Running · {t}"),
            None => "Running".into(),
        };
        let text = match (c.busy, c.background.len()) {
            (true, 0) => running,
            (true, n) => format!("{running} · {n} in background"),
            (false, 0) => "Idle".into(),
            (false, n) => format!("{n} in background"),
        };
        let (busy, started, model) = (c.busy, c.started, c.model().to_string());
        let (queued, steered, tasks) = (c.queued(), c.steered, c.background.clone());
        HoverCard::new("status-card")
            .anchor(Anchor::TopRight)
            .trigger(
                h_flex()
                    .id("status")
                    .pl_3()
                    .gap_1p5()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(div().size(px(6.)).rounded_full().bg(if c.working() {
                        theme.primary
                    } else {
                        faint(theme)
                    }))
                    .child(div().max_w(px(360.)).truncate().child(text)),
            )
            .content(move |_, _, cx| {
                let theme = cx.theme();
                let line = |t: String| div().child(t);
                v_flex()
                    .gap_1()
                    .text_xs()
                    .max_w(px(420.))
                    .when_some(started.filter(|_| busy), |d, t| {
                        d.child(line(format!(
                            "Turn running for {}",
                            crate::chat::duration(t.elapsed().as_secs())
                        )))
                    })
                    .child(line(format!("Model {model}")))
                    .when(queued > 0, |d| d.child(line(format!("{queued} queued"))))
                    .when(steered, |d| {
                        d.child(line("Message sent mid-turn, not read yet".into()))
                    })
                    .when(!tasks.is_empty(), |d| {
                        d.child(
                            div()
                                .pt_1()
                                .text_color(theme.muted_foreground)
                                .child(format!("{} in background", tasks.len())),
                        )
                        .children(tasks.iter().map(|t| {
                            h_flex()
                                .gap_1p5()
                                .child(Icon::new(IconName::Bot).with_size(px(12.)))
                                .child(t.clone())
                        }))
                    })
            })
    }

    /// The header's branch label; hovering lists each repo's branch, linked to GitHub once pushed.
    fn render_branch(&self, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let branch = self
            .active_ws()
            .map(|w| w.branch.clone())
            .unwrap_or_default();
        let links = self
            .active_sync()
            .map(|s| s.read(cx).branch_urls())
            .unwrap_or_default();
        HoverCard::new("branch-card")
            .anchor(Anchor::TopRight)
            .trigger(
                h_flex()
                    .id("branch")
                    .min_w_0()
                    .gap_1p5()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(IconName::GitBranch).with_size(px(12.)))
                    .child(div().min_w_0().truncate().child(branch.clone())),
            )
            .content(move |_, _, cx| {
                let theme = cx.theme();
                v_flex()
                    .gap_1()
                    .text_xs()
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child(format!("Branch {branch}")),
                    )
                    .children(links.iter().enumerate().map(|(ix, (repo, url))| {
                        h_flex()
                            .gap_1p5()
                            .child(Icon::new(IconName::FolderGit2).with_size(px(12.)))
                            .child(match url {
                                Some(u) => Link::new(("branch-link", ix))
                                    .href(u.clone())
                                    .child(repo.clone())
                                    .into_any_element(),
                                None => h_flex()
                                    .gap_1p5()
                                    .child(repo.clone())
                                    .child(
                                        div()
                                            .text_color(theme.muted_foreground)
                                            .child("not on GitHub"),
                                    )
                                    .into_any_element(),
                            })
                    }))
            })
    }

    fn render_diff(&self, cx: &mut Context<Self>) -> AnyElement {
        let viewed = self.viewed_now.clone();
        let shark = cx.entity().downgrade();
        let placeholder = self.diff_placeholder(cx);
        let empty = placeholder.is_some();
        let diff_with_annotations = self.with_comments(Diff::new(&self.diff), cx);
        div()
            .key_context("DiffPane")
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(self.viewport_probe())
            .on_scroll_wheel(cx.listener(|this, _, _, _| this.lose_view()))
            // The diff's own scrolling keys move it without the line cursor.
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _, _| {
                let k = ev.keystroke.key.as_str();
                if matches!(k, "up" | "down" | "pageup" | "pagedown" | "home" | "end") {
                    this.lose_view();
                }
            }))
            .on_action(cx.listener(|this, _: &SelectNext, w, cx| this.diff_move(1, None, w, cx)))
            .on_action(cx.listener(|this, _: &SelectPrev, w, cx| this.diff_move(-1, None, w, cx)))
            .on_action(cx.listener(|this, _: &PageDown, w, cx| this.diff_move(2, None, w, cx)))
            .on_action(cx.listener(|this, _: &PageUp, w, cx| this.diff_move(-2, None, w, cx)))
            .on_action(
                cx.listener(|this, _: &SelectFirst, w, cx| this.diff_move(0, Some(true), w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectLast, w, cx| this.diff_move(0, Some(false), w, cx)),
            )
            .on_action(cx.listener(|this, _: &AddComment, w, cx| this.comment_dialog(w, cx)))
            .on_action(cx.listener(|this, _: &SendComments, w, cx| this.send_comments(w, cx)))
            .on_action(cx.listener(|this, _: &NextHunk, w, cx| this.diff_change(true, w, cx)))
            .on_action(cx.listener(|this, _: &PrevHunk, w, cx| this.diff_change(false, w, cx)))
            .on_action(cx.listener(|this, _: &NextFile, _, cx| this.step_file(1, cx)))
            .on_action(cx.listener(|this, _: &PrevFile, _, cx| this.step_file(-1, cx)))
            .on_action(cx.listener(|this, _: &RefreshDiff, _, cx| this.refresh_diff(cx)))
            .on_action(cx.listener(|this, _: &ToggleDiffMode, _, cx| {
                this.diff.update(cx, |d, cx| {
                    let mode = if d.mode() == DiffMode::Split {
                        DiffMode::Unified
                    } else {
                        DiffMode::Split
                    };
                    d.set_mode(mode, cx)
                })
            }))
            .on_action(cx.listener(|this, _: &ToggleViewed, w, cx| {
                if let Some(ix) = this.cursor_file(w, cx) {
                    this.toggle_viewed(ix, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &OpenFile, w, cx| {
                if let Some(ix) = this.cursor_file(w, cx) {
                    this.open_file(ix, w, cx)
                }
            }))
            // The diff isn't drawn, so its focus handle lands here and the pane's keys still work.
            .when(empty, |d| {
                d.track_focus(&self.diff.read(cx).focus_handle(cx))
            })
            .children(placeholder)
            .when(!empty, |d| {
                d.child(
                    diff_with_annotations.render_header_prefix(move |file, _, _| {
                        // A prefix, not a suffix: the header is as wide as the longest line, so
                        // a suffix scrolls out of view.
                        let path = file.path().clone();
                        let shark = shark.clone();
                        Checkbox::new(SharedString::from(format!("viewed:{path}")))
                            .small()
                            .tooltip("Viewed (v)")
                            .checked(viewed.contains(&path))
                            .on_click(move |_, _, cx| {
                                shark
                                    .update(cx, |s, cx| s.toggle_viewed_path(&path, cx))
                                    .ok();
                            })
                    }),
                )
            })
            .into_any_element()
    }

    fn render_tree(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, muted) = (theme.border, theme.muted_foreground);
        let focused = self.tree_focus.is_focused(window);
        let n = self.diff_files.len();
        v_flex()
            .id("file-tree")
            .key_context("FileTree")
            .track_focus(&self.tree_focus)
            .w(px(260.))
            .flex_none()
            .h_full()
            .border_l_1()
            .border_color(border)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                let to = (this.tree_sel + 1).min(this.tree_rows.len().saturating_sub(1));
                this.tree_select(to, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| {
                this.tree_select(this.tree_sel.saturating_sub(1), cx)
            }))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| this.tree_select(0, cx)))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| {
                this.tree_select(this.tree_rows.len().saturating_sub(1), cx)
            }))
            .on_action(cx.listener(|this, _: &PageDown, _, cx| {
                let to = (this.tree_sel + 15).min(this.tree_rows.len().saturating_sub(1));
                this.tree_select(to, cx)
            }))
            .on_action(cx.listener(|this, _: &PageUp, _, cx| {
                this.tree_select(this.tree_sel.saturating_sub(15), cx)
            }))
            .on_action(
                cx.listener(|this, _: &Confirm, w, cx| this.tree_confirm(this.tree_sel, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &CollapseDir, _, cx| this.set_dir_open(Some(false), cx)),
            )
            .on_action(cx.listener(|this, _: &ExpandDir, _, cx| this.set_dir_open(Some(true), cx)))
            .on_action(cx.listener(|this, _: &ToggleViewed, _, cx| {
                let Some(r) = this.tree_rows.get(this.tree_sel) else {
                    return;
                };
                match r.file {
                    Some(ix) => this.toggle_viewed(ix, cx),
                    None => this.toggle_viewed_dir(&r.path.clone(), cx),
                }
            }))
            .on_action(cx.listener(|this, _: &OpenFile, w, cx| {
                if let Some(ix) = this.cursor_file(w, cx) {
                    this.open_file(ix, w, cx)
                }
            }))
            .child(
                h_flex()
                    .h(px(36.))
                    .flex_none()
                    .px_3()
                    .gap_2()
                    .text_xs()
                    .text_color(muted)
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Files"),
                    )
                    .child(format!("{}/{n} viewed", self.viewed_now.len())),
            )
            .child(
                uniform_list(
                    "file-tree-rows",
                    self.tree_rows.len(),
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|ix| this.render_tree_row(ix, focused, cx))
                            .collect()
                    }),
                )
                .track_scroll(&self.tree_scroll)
                .flex_1()
                .pb_2(),
            )
    }

    fn render_tree_row(&self, ix: usize, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let r = &self.tree_rows[ix];
        let selected = ix == self.tree_sel;
        let open_file = self.editor.as_ref().map(|e| e.read(cx).path.clone());
        let row = h_flex()
            .id(("tree-row", ix))
            .h(px(28.))
            .w_full()
            .pr_2()
            .pl(px(8. + 12. * r.depth as f32))
            .gap_1p5()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.transparent)
            .text_sm()
            .when(selected && focused, |d| {
                d.bg(theme.list_active).border_color(theme.primary)
            })
            .when(selected && !focused, |d| d.bg(theme.list_hover))
            .hover(|d| d.bg(theme.list_hover))
            .on_click(cx.listener(move |this, e: &ClickEvent, w, cx| {
                this.tree_focus.focus(w, cx);
                match (e.click_count(), this.tree_rows.get(ix).and_then(|r| r.file)) {
                    (2, Some(file)) => this.open_file(file, w, cx),
                    _ => this.tree_confirm(ix, w, cx),
                }
            }));
        let Some(file) = r.file else {
            let open = !self.collapsed_dirs.contains(&r.path);
            return div()
                .px_2()
                .child(
                    row.text_color(theme.muted_foreground)
                        .child(
                            Icon::new(if open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .with_size(px(14.)),
                        )
                        .child(div().flex_1().truncate().child(r.name.clone()))
                        .child(self.dir_viewed_box(ix, &r.path, cx)),
                )
                .into_any_element();
        };
        let f = &self.diff_files[file];
        let state = self.viewed(file);
        let icon = match f.status() {
            DiffFileStatus::Added => IconName::FilePlus,
            DiffFileStatus::Deleted => IconName::FileMinus,
            _ => IconName::FileDiff,
        };
        let is_open = open_file.as_deref() == Some(f.path().as_str());
        let row = row
            .child(
                Icon::new(icon)
                    .with_size(px(14.))
                    .text_color(theme.muted_foreground),
            )
            .child(
                div()
                    .flex_1()
                    .truncate()
                    .text_color(match state {
                        Viewed::Yes => theme.muted_foreground,
                        _ => theme.foreground,
                    })
                    .when(is_open, |d| d.font_weight(FontWeight::MEDIUM))
                    .child(r.name.clone()),
            )
            .when(state == Viewed::Changed, |d| {
                d.child(
                    div()
                        .id(("changed", ix))
                        .size(px(6.))
                        .rounded_full()
                        .bg(theme.warning_foreground)
                        .tooltip(|w, cx| Tooltip::new("Changed since viewed").build(w, cx)),
                )
            })
            .child(
                h_flex()
                    .gap_1()
                    .text_xs()
                    .when(f.additions() > 0, |d| {
                        d.child(
                            div()
                                .text_color(theme.success_foreground)
                                .child(format!("+{}", f.additions())),
                        )
                    })
                    .when(f.deletions() > 0, |d| {
                        d.child(
                            div()
                                .text_color(theme.danger_foreground)
                                .child(format!("−{}", f.deletions())),
                        )
                    }),
            )
            .child(
                div()
                    .id(("viewed-box", ix))
                    // The row's click selects; this one only toggles.
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        Checkbox::new(("viewed", ix))
                            .small()
                            .checked(state == Viewed::Yes)
                            .tooltip("Viewed (v)")
                            .on_click(cx.listener(move |this, _: &bool, _, cx| {
                                this.toggle_viewed(file, cx)
                            })),
                    ),
            );
        div().px_2().child(row).into_any_element()
    }

    fn dir_viewed_box(&self, ix: usize, dir: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let files = self.dir_files(dir);
        let all = files.iter().all(|&f| self.viewed(f) == Viewed::Yes);
        let dir = dir.to_string();
        div()
            .id(("dir-viewed-box", ix))
            // The row's click folds the folder; this one only toggles.
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                Checkbox::new(("dir-viewed", ix))
                    .small()
                    .checked(all)
                    .tooltip("All viewed (v)")
                    .on_click(
                        cx.listener(move |this, _: &bool, _, cx| this.toggle_viewed_dir(&dir, cx)),
                    ),
            )
    }

    fn render_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(editor) = self.editor.clone() else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let e = editor.read(cx);
        let path = e.path.clone();
        let ix = self.diff_files.iter().position(|f| *f.path() == path);
        let viewed = ix.map(|ix| self.viewed(ix));
        let (dir, name) = match path.rsplit_once('/') {
            Some((d, n)) => (format!("{d}/"), n.to_string()),
            None => (String::new(), path.clone()),
        };
        let vim = e.vim_on.then(|| {
            let p = e.vim.pending();
            match p.is_empty() {
                true => e.vim.status().to_string(),
                false => format!("{} {p}", e.vim.status()),
            }
        });
        let (dirty, conflict) = (e.dirty, e.conflict);
        // Leader keys (`space …`) work in Vim's Normal mode, where space types nothing.
        let normal =
            e.vim_on && e.vim.mode == crate::vim::Mode::Normal && e.vim.pending().is_empty();
        v_flex()
            .key_context(if normal {
                "FileEditor VimNormal"
            } else {
                "FileEditor"
            })
            .size_full()
            .border_l_1()
            .border_color(theme.border)
            .on_action(cx.listener(|this, _: &SaveFile, w, cx| {
                if let Some(e) = this.editor.clone() {
                    e.update(cx, |e, cx| e.save(false, w, cx))
                }
            }))
            .on_action(cx.listener(|this, _: &CloseFile, w, cx| this.close_file(w, cx)))
            .child(
                h_flex()
                    .h(px(36.))
                    .flex_none()
                    .px_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_sm()
                    .child(
                        Icon::new(IconName::FileCode)
                            .with_size(px(14.))
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme.muted_foreground)
                                    .child(dir),
                            )
                            .child(div().flex_none().child(name))
                            .when(dirty, |d| {
                                d.child(
                                    div()
                                        .id("dirty")
                                        .ml_2()
                                        .size(px(6.))
                                        .rounded_full()
                                        .bg(theme.foreground)
                                        .tooltip(|w, cx| {
                                            Tooltip::new("Unsaved (cmd-s)").build(w, cx)
                                        }),
                                )
                            }),
                    )
                    .child(
                        div()
                            .id("vim-pill")
                            .px_2()
                            .h(px(20.))
                            .flex()
                            .items_center()
                            .rounded_full()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .when(vim.is_some(), |d| d.bg(theme.secondary))
                            .hover(|d| d.text_color(theme.foreground))
                            .child(vim.unwrap_or_else(|| "VIM OFF".into()))
                            .tooltip(|w, cx| {
                                Tooltip::new("Toggle Vim keys (ctrl-cmd-v)").build(w, cx)
                            })
                            .on_click(cx.listener(|this, _, w, cx| this.toggle_vim(w, cx))),
                    )
                    .when_some(viewed, |d, v| {
                        let ix = ix.unwrap_or_default();
                        d.child(
                            Checkbox::new("editor-viewed")
                                .label("Viewed")
                                .small()
                                .checked(v == Viewed::Yes)
                                .on_click(cx.listener(move |this, _: &bool, _, cx| {
                                    this.toggle_viewed(ix, cx)
                                })),
                        )
                    })
                    .child(
                        Button::new("close-file")
                            .ghost()
                            .xsmall()
                            .icon(IconName::X)
                            .tooltip("Close file (cmd-w)")
                            .on_click(cx.listener(|this, _, w, cx| this.close_file(w, cx))),
                    ),
            )
            .when(conflict, |d| {
                let (e1, e2) = (editor.clone(), editor.clone());
                d.child(
                    h_flex()
                        .h(px(36.))
                        .flex_none()
                        .px_3()
                        .gap_2()
                        .border_b_1()
                        .border_color(theme.border)
                        .text_sm()
                        .child(
                            Icon::new(IconName::TriangleAlert)
                                .with_size(px(14.))
                                .text_color(theme.warning_foreground),
                        )
                        .child(
                            div()
                                .flex_1()
                                .child("Changed on disk since you started editing"),
                        )
                        .child(
                            Button::new("reload")
                                .small()
                                .outline()
                                .label("Reload")
                                .on_click(move |_, w, cx| e1.update(cx, |e, cx| e.discard(w, cx))),
                        )
                        .child(
                            Button::new("overwrite")
                                .small()
                                .ghost()
                                .label("Keep mine")
                                .on_click(move |_, w, cx| {
                                    e2.update(cx, |e, cx| e.save(true, w, cx))
                                }),
                        ),
                )
            })
            .child(div().flex_1().min_h_0().child(editor))
            .into_any_element()
    }

    fn step_file(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.diff_files.is_empty() {
            return;
        }
        let n = self.diff_files.len() as isize;
        self.diff_file = (self.diff_file as isize + delta).rem_euclid(n) as usize;
        let path = self.diff_files[self.diff_file].path().to_string();
        self.review.at_file(&path);
        self.diff.update(cx, |d, cx| d.scroll_to_file(&path, cx));
        if let Some(row) = self
            .tree_rows
            .iter()
            .position(|r| r.file == Some(self.diff_file))
        {
            self.tree_sel = row;
            self.tree_scroll
                .scroll_to_item(row, ScrollStrategy::Nearest);
        }
        cx.notify();
    }
}

impl Render for Shark {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .key_context("Shark")
            .size_full()
            .bg(theme.sidebar)
            .text_color(theme.foreground)
            .font_features(inter_features())
            .on_action(cx.listener(|this, _: &TogglePalette, w, cx| this.toggle_palette(w, cx)))
            .on_action(cx.listener(|this, _: &NewWorkspace, w, cx| this.new_workspace(w, cx)))
            .on_action(cx.listener(|this, _: &ArchiveWorkspace, w, cx| this.archive(false, w, cx)))
            .on_action(
                cx.listener(|this, _: &ForceArchiveWorkspace, w, cx| this.archive(true, w, cx)),
            )
            .on_action(cx.listener(|this, _: &AddRepo, w, cx| this.add_repo(w, cx)))
            .on_action(cx.listener(|this, _: &AddRepoToWorkspace, w, cx| {
                this.pick_repo_for_workspace(w, cx)
            }))
            .on_action(cx.listener(|this, _: &StopAgent, _, cx| this.stop(cx)))
            .on_action(cx.listener(|this, _: &FocusQueue, w, cx| this.focus_queue(w, cx)))
            .on_action(cx.listener(|this, _: &ShowChat, w, cx| this.show(Tab::Chat, w, cx)))
            .on_action(cx.listener(|this, _: &NewChat, w, cx| this.new_chat(w, cx)))
            .on_action(cx.listener(|this, _: &CloseChat, w, cx| this.close_chat(w, cx)))
            .on_action(cx.listener(|this, _: &ReopenChat, w, cx| this.reopen_chat(None, w, cx)))
            .on_action(cx.listener(|this, _: &NextChat, w, cx| this.cycle_chat(1, w, cx)))
            .on_action(cx.listener(|this, _: &PrevChat, w, cx| this.cycle_chat(-1, w, cx)))
            .on_action(cx.listener(|this, _: &OpenSide, w, cx| this.open_side(w, cx)))
            .on_action(cx.listener(|this, _: &ShowDiff, w, cx| this.show(Tab::Diff, w, cx)))
            .on_action(cx.listener(|this, _: &CreatePrs, w, cx| this.create_prs(w, cx)))
            .on_action(cx.listener(|this, _: &OpenPrs, w, cx| this.open_prs(w, cx)))
            .on_action(cx.listener(|this, _: &CreateDraftPrs, w, cx| this.create_draft_prs(w, cx)))
            .on_action(cx.listener(|this, _: &MergeEarlier, w, cx| {
                this.with_sync(w, cx, |g, w, cx| g.move_in_merge_order(true, w, cx))
            }))
            .on_action(cx.listener(|this, _: &MergeLater, w, cx| {
                this.with_sync(w, cx, |g, w, cx| g.move_in_merge_order(false, w, cx))
            }))
            .on_action(
                cx.listener(|this, _: &GitNext, w, cx| this.with_sync(w, cx, GitSync::primary)),
            )
            .on_action(
                cx.listener(|this, _: &GitMenu, w, cx| this.with_sync(w, cx, GitSync::open_menu)),
            )
            .on_action(
                cx.listener(|this, _: &GitFetch, w, cx| this.with_sync(w, cx, GitSync::fetch_now)),
            )
            .on_action(cx.listener(|this, _: &PickModel, w, cx| this.pick_model(w, cx)))
            .on_action(cx.listener(|this, _: &RevealInFinder, w, cx| {
                this.with_target(w, cx, |ws, _, cx| cx.reveal_path(&ws.dir()))
            }))
            .on_action(cx.listener(|this, _: &OpenInTerminal, w, cx| {
                this.with_target(w, cx, |ws, _, _| open_terminal(&ws.dir()))
            }))
            .on_action(cx.listener(|this, _: &CopyPath, w, cx| {
                this.with_target(w, cx, |ws, w, cx| {
                    copy(ws.dir().display().to_string(), w, cx)
                })
            }))
            .on_action(cx.listener(|this, _: &CopyBranch, w, cx| {
                this.with_target(w, cx, |ws, w, cx| copy(ws.branch.clone(), w, cx))
            }))
            .on_action(cx.listener(|this, a: &SetModel, w, cx| this.set_model(a.0, w, cx)))
            .on_action(cx.listener(|this, _: &NextAttention, w, cx| this.next_attention(w, cx)))
            .on_action(cx.listener(|this, a: &OpenWorkspace, w, cx| {
                if let Some(ix) = this.ws_order().get(a.0).copied() {
                    this.open_workspace(ix, w, cx)
                }
            }))
            .on_action(
                cx.listener(|this, _: &NewSection, w, cx| this.start_naming(None, None, w, cx)),
            )
            .on_action(cx.listener(|this, _: &MoveToSection, w, cx| this.pick_section(w, cx)))
            .on_action(cx.listener(|this, _: &RenameSection, w, cx| {
                this.with_section(w, cx, |s, i, w, cx| s.start_naming(Some(i), None, w, cx))
            }))
            .on_action(cx.listener(|this, _: &SectionIcon, w, cx| {
                this.with_section(w, cx, Shark::pick_icon)
            }))
            .on_action(cx.listener(|this, _: &DeleteSection, w, cx| {
                this.with_section(w, cx, Shark::delete_section)
            }))
            // The sidebar swaps status icons for cmd-N hints while cmd is held.
            .on_modifiers_changed(cx.listener(|_, _, _, cx| cx.notify()))
            .on_action(cx.listener(|this, _: &FocusLeft, w, cx| this.move_focus(-1, w, cx)))
            .on_action(cx.listener(|this, _: &FocusRight, w, cx| this.move_focus(1, w, cx)))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            .on_action(cx.listener(|this, _: &ToggleFileTree, w, cx| this.toggle_tree(w, cx)))
            .on_action(cx.listener(|this, _: &ToggleVim, w, cx| this.toggle_vim(w, cx)))
            .on_action(cx.listener(|this, _: &CloseFile, w, cx| this.close_file(w, cx)))
            .on_action(cx.listener(|this, _: &OpenAnyFile, w, cx| this.open_any_file(w, cx)))
            .on_action(cx.listener(|this, _: &OpenSettings, w, cx| {
                crate::settings::open(&this.state, w, cx)
            }))
            .child(self.render_sidebar(window, cx))
            .child(
                // Linear's "island": content floats on the sidebar color, inset and rounded.
                div().flex_1().min_w_0().h_full().py_2().pr_2().child(
                    h_flex()
                        .size_full()
                        .rounded(cx.theme().radius_lg)
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().background)
                        .overflow_hidden()
                        .child(self.render_main(window, cx)),
                ),
            )
    }
}

/// Contents of the New workspace dialog: a branch name and a filterable repo picker.
/// Starts with the last-used repos ticked and floated to the top, so the common case
/// is type a branch, enter.
struct NewWorkspaceForm {
    branch: Entity<InputState>,
    /// Optional first message, sent once the workspace is created.
    prompt: Entity<InputState>,
    picker: Entity<CommandState>,
    repos: Vec<Repo>,
    picked: Vec<bool>,
    shark: WeakEntity<Shark>,
    _sub: Subscription,
}

impl NewWorkspaceForm {
    fn new(
        mut repos: Vec<Repo>,
        last: &[String],
        shark: WeakEntity<Shark>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let branch = cx
            .new(|cx| InputState::new(window, cx).placeholder("branch, e.g. kasper/eng-123-login"));
        // Branch names can't contain spaces; turn them into dashes as you type.
        let _sub = cx.subscribe_in(&branch, window, |_, input, ev: &InputEvent, window, cx| {
            if let InputEvent::Change = ev {
                let value = input.read(cx).value();
                if value.contains(' ') {
                    let fixed = value.replace(' ', "-");
                    input.update(cx, |i, cx| i.set_value(fixed, window, cx));
                }
            }
        });
        let only = repos.len() == 1;
        let was_picked = |r: &Repo| only || last.contains(&r.name);
        repos.sort_by_key(|r| !was_picked(r)); // stable: picked first, otherwise in added order
        let picked = repos.iter().map(was_picked).collect();
        NewWorkspaceForm {
            branch,
            prompt: cx.new(|cx| {
                InputState::new(window, cx).placeholder("First message to the agent (optional)")
            }),
            picker: cx.new(|cx| CommandState::new(window, cx)),
            repos,
            picked,
            shark,
            _sub,
        }
    }

    fn toggle(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(p) = self.picked.get_mut(ix) {
            *p = !*p;
            cx.notify();
        }
    }

    fn picked_repos(&self) -> Vec<Repo> {
        self.repos
            .iter()
            .zip(&self.picked)
            .filter(|(_, p)| **p)
            .map(|(r, _)| r.clone())
            .collect()
    }

    /// Returns whether the form was valid (and the dialog may close).
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let branch = self.branch.read(cx).value().trim().to_string();
        let repos = self.picked_repos();
        if branch.is_empty() || repos.is_empty() {
            let (msg, focus_branch) = match branch.is_empty() {
                true => ("Name the branch first", true),
                false => ("Pick at least one repo (space or enter ticks it)", false),
            };
            Shark::notify_err(window, cx, msg);
            match focus_branch {
                true => self.branch.update(cx, |i, cx| i.focus(window, cx)),
                false => self.picker.update(cx, |p, cx| p.focus(window, cx)),
            }
            return false;
        }
        let prompt = self.prompt.read(cx).value().trim().to_string();
        let prompt = (!prompt.is_empty()).then_some(prompt);
        self.shark
            .update(cx, |s, cx| {
                s.create_workspace(branch, repos, prompt, window, cx)
            })
            .ok();
        true
    }
}

impl Render for NewWorkspaceForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, faint) = (theme.muted_foreground, faint(theme));
        let in_branch = self.branch.read(cx).focus_handle(cx).is_focused(window)
            || self.prompt.read(cx).focus_handle(cx).is_focused(window);
        let items = self.repos.iter().zip(&self.picked).map(|(r, p)| {
            CommandItem::new()
                .label(r.name.clone())
                .icon(IconName::FolderGit2)
                .keywords([r.path.to_string_lossy().to_string()])
                .checked(*p)
        });
        let entity = cx.entity().downgrade();
        let picker = self.picker.clone();
        let count = self.picked.iter().filter(|p| **p).count();
        let key = |k: &str| Kbd::new(Keystroke::parse(k).unwrap());
        let hint = |keys: Vec<Kbd>, label: &'static str| {
            h_flex()
                .gap_1()
                .children(keys)
                .child(div().pl_0p5().child(label))
        };
        v_flex()
            .key_context("NewWorkspace")
            .gap_3()
            .on_action(cx.listener(|this, _: &SwitchField, window, cx| {
                // branch -> repos -> first message -> branch
                if this.branch.read(cx).focus_handle(cx).is_focused(window) {
                    this.picker.update(cx, |p, cx| p.focus(window, cx))
                } else if this.prompt.read(cx).focus_handle(cx).is_focused(window) {
                    this.branch.update(cx, |i, cx| i.focus(window, cx))
                } else {
                    this.prompt.update(cx, |i, cx| i.focus(window, cx))
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleRepo, _, cx| {
                if let Some(ix) = this.picker.read(cx).selected_index() {
                    this.toggle(ix.row, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SubmitForm, window, cx| {
                if this.submit(window, cx) {
                    window.close_dialog(cx);
                }
            }))
            .child(Input::new(&self.branch))
            .child(
                h_flex()
                    .text_xs()
                    .text_color(muted)
                    .child(div().flex_1().child("Repos"))
                    .child(format!("{count} of {}", self.repos.len())),
            )
            .child(
                Command::new(&self.picker)
                    .items(items)
                    .placeholder("Filter repos…")
                    .bordered(true)
                    .max_h(px(240.))
                    // Enter ticks the highlighted repo and clears the filter for the next one.
                    .on_confirm(move |ix, window, cx| {
                        entity.update(cx, |f, cx| f.toggle(ix.row, cx)).ok();
                        picker.update(cx, |p, cx| p.set_query("", window, cx));
                    }),
            )
            .child(Input::new(&self.prompt))
            .child(
                h_flex()
                    .gap_3()
                    .text_xs()
                    .text_color(faint)
                    .child(hint(vec![key("tab")], "switch field"))
                    .child(match in_branch {
                        true => hint(vec![key("enter")], "create"),
                        false => hint(vec![key("space")], "tick repo"),
                    })
                    .child(div().flex_1())
                    .child(
                        Button::new("create")
                            .primary()
                            .label("Create")
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.submit(window, cx) {
                                    window.close_dialog(cx);
                                }
                            })),
                    ),
            )
    }
}

#[cfg(all(test, feature = "test-support"))]
pub(crate) mod ui_tests {
    use super::*;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin
    use gpui_kit::test::{TestAppContextExt, TestWindowExt};
    use std::sync::Mutex;
    use std::time::Instant;
    use std::{fs, path::PathBuf, time::Duration};

    /// UI tests share the process-wide `TEST_HOME` and `state.json`, so they take turns. Chat
    /// tests that make workspace dirs take it too: `test_home` wipes them.
    pub(crate) static UI_LOCK: Mutex<()> = Mutex::new(());

    fn test_home() -> PathBuf {
        let home = crate::state::TEST_HOME
            .get_or_init(|| std::env::temp_dir().join(format!("shark-ui-{}", std::process::id())))
            .clone();
        let _ = fs::remove_dir_all(&home);
        home
    }

    fn init_repo(repo: &std::path::Path) {
        fs::create_dir_all(repo).unwrap();
        git::git(repo, &["init", "-q", "-b", "main"]).unwrap();
        git::git(
            repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "i",
            ],
        )
        .unwrap();
    }

    /// Saves `state`, then starts the app on the real keymap.
    fn boot(cx: &mut TestAppContext, state: State) -> (AnyWindowHandle, Entity<Shark>) {
        state.save();
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::bind_keys(cx);
        });
        open(cx)
    }

    fn open(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Shark>) {
        cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
                cx.new(|cx| Shark::new(w, cx))
            })
        })
        .unwrap()
    }

    /// `on(cx, f)` runs `f` on the app, then lets it settle.
    fn on_app(
        window: AnyWindowHandle,
        shark: Entity<Shark>,
    ) -> impl Fn(&mut TestAppContext, &dyn Fn(&mut Shark, &mut Window, &mut Context<Shark>)) {
        move |cx, f| {
            cx.update_window(window, |_, w, cx| shark.update(cx, |s, cx| f(s, w, cx)))
                .unwrap();
            cx.run_until_parked();
        }
    }

    /// Settings apply and persist at once; an edited prompt reaches the git button, reset drops it.
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn settings_apply_and_save(cx: &mut TestAppContext) {
        use crate::settings::Settings;
        use crate::state::Mode;
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        test_home();
        let (window, shark) = boot(cx, State::default());
        cx.update_window(window, |_, w, cx| {
            let state = shark.read(cx).state.clone();
            let weak = shark.downgrade();
            let view = cx.new(|cx| Settings::new(weak, &state, w, cx));
            view.update(cx, |v, cx| {
                v.appearance(cx, |a| {
                    a.mode = Mode::Dark;
                    a.dark = "Sequoia".into();
                    a.mono_font = Some("Menlo".into());
                });
                v.set_prompt(0, Some("ship {repos}".into()), cx);
            });
            assert_eq!(cx.theme().theme_name().as_ref(), "Sequoia Dark");
            // Headless tests have no system fonts, so check the config, not the resolved family.
            let mono = |t: &gpui_kit::component::ThemeConfig| t.mono_font_family.clone();
            assert_eq!(mono(&cx.theme().dark_theme).as_deref(), Some("Menlo"));
            let saved = State::load();
            assert_eq!(saved.appearance.dark, "Sequoia");
            assert_eq!(saved.prompts["commit"], "ship {repos}");
            assert_eq!(crate::prompts::render("commit", "api", "b"), "ship api");

            view.update(cx, |v, cx| {
                v.appearance(cx, |a| a.mode = Mode::Light);
                v.reset_prompt(0, w, cx);
            });
            assert_eq!(cx.theme().theme_name().as_ref(), "Linear Light");
            // The font override survives the mode switch.
            assert_eq!(mono(&cx.theme().light_theme).as_deref(), Some("Menlo"));
            assert!(State::load().prompts.is_empty());
            assert!(crate::prompts::render("commit", "api", "b").starts_with("Commit and push"));
        })
        .unwrap();
    }

    /// Drives the real keymap headlessly: create a workspace, vim focus moves, tabs, archive.
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)] // each test owns its thread; the lock only orders them
    async fn keyboard_flow(cx: &mut TestAppContext) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = test_home();
        let repos: Vec<Repo> = ["api", "web"]
            .map(|name| {
                let path = home.join("src").join(name);
                init_repo(&path);
                Repo {
                    name: name.into(),
                    path,
                }
            })
            .into();
        let (window, shark) = boot(
            cx,
            State {
                repos,
                workspaces: vec![],
                vim: false,
                last_repos: vec!["api".into()],
                ..Default::default()
            },
        );
        let press = |cx: &mut TestAppContext, keys: &[&str]| {
            cx.update_window(window, |_, w, cx| keys.iter().for_each(|k| w.press(k, cx)))
                .unwrap()
        };

        press(cx, &["space", "w", "n"]);
        cx.update_window(window, |_, w, cx| w.input("ui x", cx))
            .unwrap();
        press(cx, &["enter"]);
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            shark.read(cx).active.is_some()
        })
        .await;
        let ws_dir = home.join("shark/workspaces/ui-x");
        assert!(ws_dir.join("api").exists() && ws_dir.join("CLAUDE.md").exists());
        assert!(
            !ws_dir.join("web").exists(),
            "only the last-used repos start ticked"
        );

        // cmd-N opens the Nth workspace; past the end does nothing.
        press(cx, &["cmd-9", "cmd-1"]);
        cx.update(|cx| assert_eq!(shark.read(cx).active.as_deref(), Some("ui-x")));

        // Opening a workspace focuses the composer: vim keys must type, not navigate.
        cx.update_window(window, |_, w, cx| w.input("jk g", cx))
            .unwrap();
        let chat = cx.update(|cx| shark.read(cx).chats["ui-x"].clone());
        cx.update(|cx| assert_eq!(chat.read(cx).composer.read(cx).value().as_ref(), "jk g"));

        // Chat tabs: cmd-t opens one next to the current chat, ctrl-tab cycles, cmd-w closes,
        // cmd-shift-t brings back a closed chat that had a session.
        let tabs = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                let ws = &shark.read(cx).state.workspaces[0];
                let ids: Vec<String> = ws.chats.iter().map(|c| c.id.clone()).collect();
                (ids, ws.active_chat, ws.closed.len())
            })
        };
        press(cx, &["cmd-t"]);
        assert_eq!(tabs(cx), (vec!["ui-x".into(), "ui-x~2".into()], 1, 0));
        press(cx, &["ctrl-tab"]);
        assert_eq!(tabs(cx).1, 0);
        cx.update(|cx| assert_eq!(chat.read(cx).composer.read(cx).value().as_ref(), "jk g"));
        press(cx, &["ctrl-tab"]);
        let second = cx.update(|cx| shark.read(cx).chats["ui-x~2"].clone());
        cx.update(|cx| {
            second.update(cx, |c, cx| {
                c.meta.session_id = Some("s2".into());
                cx.emit(ChatEvent::Meta);
            })
        });
        press(cx, &["cmd-w"]);
        assert_eq!(tabs(cx), (vec!["ui-x".into()], 0, 1));
        cx.update(|cx| assert!(!shark.read(cx).chats.contains_key("ui-x~2")));
        press(cx, &["cmd-shift-t"]);
        assert_eq!(tabs(cx), (vec!["ui-x".into(), "ui-x~2".into()], 1, 0));
        cx.update(|cx| {
            let ws = &shark.read(cx).state.workspaces[0];
            assert_eq!(
                ws.chats[1].session_id.as_deref(),
                Some("s2"),
                "reopens the session"
            );
        });
        press(cx, &["cmd-t"]);
        press(cx, &["cmd-w"]); // an untouched chat isn't kept for reopening
        assert_eq!(tabs(cx), (vec!["ui-x".into(), "ui-x~2".into()], 1, 0));
        press(cx, &["cmd-w"]);
        assert_eq!(tabs(cx), (vec!["ui-x".into()], 0, 1));
        cx.update(|cx| assert_eq!(chat.read(cx).composer.read(cx).value().as_ref(), "jk g"));

        // Model shortcuts work while typing and keep the draft. Let the New workspace
        // dialog's deferred focus restore run first (it targets the sidebar), then go back.
        cx.executor().advance_clock(Duration::from_secs(1));
        press(cx, &["ctrl-w", "l", "i"]);
        let model = |cx: &mut TestAppContext| {
            cx.update(|cx| shark.read(cx).state.workspaces[0].chats[0].model.clone())
        };
        press(cx, &["ctrl-cmd-3"]);
        assert_eq!(model(cx).as_deref(), Some("opus"));
        press(cx, &["cmd-/"]);
        cx.run_until_parked();
        press(cx, &["enter"]);
        assert_eq!(
            model(cx).as_deref(),
            Some("opus"),
            "menu opens on the current model"
        );
        press(cx, &["cmd-/"]);
        cx.update_window(window, |_, w, cx| w.input("son", cx))
            .unwrap();
        press(cx, &["enter"]);
        assert_eq!(model(cx).as_deref(), Some("sonnet"));
        cx.update(|cx| assert_eq!(chat.read(cx).composer.read(cx).value().as_ref(), "jk g"));

        press(cx, &["escape"]);
        cx.update_window(window, |_, w, cx| {
            assert!(chat.read(cx).focus.is_focused(w), "escape -> transcript")
        })
        .unwrap();
        press(cx, &["ctrl-w", "h"]);
        cx.update_window(window, |_, w, cx| {
            assert!(
                shark.read(cx).sidebar_focus.is_focused(w),
                "ctrl-w h -> sidebar"
            )
        })
        .unwrap();
        press(cx, &["space", "d"]);
        cx.update(|cx| assert_eq!(shark.read(cx).tab, Tab::Diff));
        press(cx, &["space", "c"]);
        cx.update(|cx| assert_eq!(shark.read(cx).tab, Tab::Chat));

        // The git button follows the worktree: clean, then dirty within a poll tick.
        let sync = cx.update(|cx| shark.read(cx).syncs["ui-x"].clone());
        let label = |cx: &App| sync.read(cx).action.as_ref().map(|a| a.label.clone());
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            label(cx).as_deref() == Some("No changes")
        })
        .await;
        fs::write(ws_dir.join("api/new.txt"), "x\n").unwrap();
        cx.executor().advance_clock(Duration::from_secs(4));
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            label(cx).as_deref() == Some("Commit and push")
        })
        .await;

        // Diff tree: the new file is listed and can be marked viewed from the tree.
        let refresh = |cx: &mut TestAppContext| {
            cx.update(|cx| shark.update(cx, |s, cx| s.refresh_diff(cx)));
            cx.run_until_parked();
        };
        press(cx, &["space", "d"]);
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            shark.read(cx).diff_files.len() == 1
        })
        .await;
        cx.update(|cx| {
            let names: Vec<String> = shark
                .read(cx)
                .tree_rows
                .iter()
                .map(|r| r.name.clone())
                .collect();
            assert_eq!(names, ["api", "new.txt"]);
        });
        press(cx, &["ctrl-w", "l"]);
        cx.update_window(window, |_, w, cx| {
            assert!(
                shark.read(cx).tree_focus.is_focused(w),
                "ctrl-w l from the diff -> tree"
            )
        })
        .unwrap();
        press(cx, &["j", "v"]);
        let viewed = |cx: &mut TestAppContext| cx.update(|cx| shark.read(cx).viewed(0));
        assert_eq!(viewed(cx), Viewed::Yes);
        assert!(
            State::load().workspaces[0]
                .viewed
                .contains_key("api/new.txt"),
            "viewed is saved"
        );
        refresh(cx);
        assert_eq!(viewed(cx), Viewed::Yes, "an unchanged file stays viewed");
        let file = ws_dir.join("api/new.txt");
        fs::write(&file, "x\ny\n").unwrap();
        refresh(cx);
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            shark.read(cx).viewed(0) == Viewed::Changed
        })
        .await;
        // `v` on a folder ticks every file in it, then unticks them all.
        press(cx, &["k", "v"]);
        assert_eq!(viewed(cx), Viewed::Yes);
        press(cx, &["v", "j"]);
        assert_eq!(viewed(cx), Viewed::No);

        // `o` opens the file next to the chat and shrinks the sidebar to a rail.
        press(cx, &["o"]);
        let editor = cx.update(|cx| shark.read(cx).editor.clone().expect("editor opened"));
        let text = |cx: &mut TestAppContext| {
            cx.update(|cx| editor.read(cx).input.read(cx).value().to_string())
        };
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            editor.read(cx).input.read(cx).value().as_ref() == "x\ny\n"
        })
        .await;
        cx.update(|cx| {
            let s = shark.read(cx);
            assert!(s.rail && s.tab == Tab::File);
        });

        // Plain editing and cmd-s.
        cx.update_window(window, |_, w, cx| w.input("z", cx))
            .unwrap();
        assert_eq!(text(cx), "zx\ny\n");
        cx.update(|cx| assert!(editor.read(cx).dirty));
        press(cx, &["cmd-s"]);
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            !editor.read(cx).dirty
        })
        .await;
        assert_eq!(fs::read_to_string(&file).unwrap(), "zx\ny\n");

        // Vim keys: normal mode edits, never inserts; the toggle survives a restart.
        press(cx, &["ctrl-cmd-v"]);
        assert!(State::load().vim, "vim toggle is saved");
        press(cx, &["d", "d"]);
        assert_eq!(text(cx), "y\n");
        press(cx, &["u"]);
        cx.run_until_parked();
        assert_eq!(text(cx), "zx\ny\n");
        press(cx, &["j", "q"]);
        assert_eq!(text(cx), "zx\ny\n", "normal mode never types");
        press(cx, &["shift-a", "!", "escape"]);
        assert_eq!(text(cx), "zx\ny!\n");
        // App chords still work in normal mode: `h`/`l` after `ctrl-w` aren't Vim motions.
        press(cx, &["ctrl-w", "l"]);
        cx.update_window(window, |_, w, cx| {
            assert!(
                shark.read(cx).tree_focus.is_focused(w),
                "ctrl-w l from the editor"
            )
        })
        .unwrap();
        press(cx, &["ctrl-w", "h"]);
        cx.update_window(window, |_, w, cx| {
            assert!(
                editor.read(cx).is_focused(w, cx),
                "ctrl-w h back to the editor"
            )
        })
        .unwrap();
        assert_eq!(text(cx), "zx\ny!\n");
        // Leader keys work in Normal mode: space t hides and shows the tree, typing nothing.
        press(cx, &["space", "t"]);
        cx.update(|cx| assert!(!shark.read(cx).show_tree, "space t in the editor"));
        press(cx, &["space", "t"]);
        assert_eq!(text(cx), "zx\ny!\n");

        // The agent writes the file while it has unsaved edits: offer reload, don't clobber.
        fs::write(&file, "agent\n").unwrap();
        cx.executor().advance_clock(Duration::from_secs(3));
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            editor.read(cx).conflict
        })
        .await;
        press(cx, &[":", "w", "enter"]);
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "agent\n",
            "save refuses to overwrite"
        );

        // Escape leaves the editor; closing asks before dropping edits.
        press(cx, &["escape"]);
        cx.update_window(window, |_, w, cx| {
            assert!(shark.read(cx).tree_focus.is_focused(w))
        })
        .unwrap();
        press(cx, &["space", "x"]);
        cx.update(|cx| assert!(shark.read(cx).editor.is_some(), "close waits for confirm"));
        press(cx, &["enter"]);
        cx.update(|cx| {
            let s = shark.read(cx);
            assert!(s.editor.is_none() && !s.rail && s.tab == Tab::Diff);
        });

        press(cx, &["ctrl-w", "h", "ctrl-w", "h", "space", "w", "shift-d"]);
        cx.update(|cx| {
            assert_eq!(
                shark.read(cx).state.workspaces.len(),
                1,
                "force archive waits for confirm"
            )
        });
        press(cx, &["enter"]);
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            shark.read(cx).state.workspaces.is_empty()
        })
        .await;
        assert!(!ws_dir.exists());

        // Repo picker: tab to the filter, enter ticks the match, space unticks the
        // highlighted (top) row, cmd-enter creates from the picker.
        press(cx, &["cmd-n"]);
        cx.run_until_parked();
        cx.update_window(window, |_, w, cx| w.input("kb", cx))
            .unwrap();
        press(cx, &["tab"]);
        cx.update_window(window, |_, w, cx| w.input("web", cx))
            .unwrap();
        press(cx, &["enter"]);
        cx.run_until_parked();
        press(cx, &["space", "cmd-enter"]);
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            !shark.read(cx).state.workspaces.is_empty()
        })
        .await;
        cx.update(|cx| {
            let s = shark.read(cx);
            let names: Vec<&str> = s.state.workspaces[0]
                .repos
                .iter()
                .map(|r| r.name.as_str())
                .collect();
            assert_eq!(names, ["web"]);
            assert_eq!(s.state.last_repos, ["web"]);
        });

        // Sections: name inline, move with `m`, fold with h/l, delete keeps the workspace.
        press(cx, &["ctrl-w", "h", "space", "s", "n"]);
        cx.run_until_parked();
        cx.update_window(window, |_, w, cx| w.input("Later", cx))
            .unwrap();
        press(cx, &["enter"]);
        let sections =
            |cx: &mut TestAppContext| cx.update(|cx| shark.read(cx).state.sections.clone());
        assert_eq!(sections(cx)[0].name, "Later");
        cx.update_window(window, |_, w, cx| {
            let s = shark.read(cx);
            assert!(s.sidebar_focus.is_focused(w) && s.sel == SideRow::Section(0));
        })
        .unwrap();
        press(cx, &["j", "m"]);
        cx.run_until_parked();
        press(cx, &["enter"]);
        assert_eq!(sections(cx)[0].workspaces, ["kb"]);
        press(cx, &["j", "h"]);
        cx.update(|cx| {
            let s = shark.read(cx);
            assert!(s.state.sections[0].collapsed && s.sel == SideRow::Section(0));
            assert!(s.ws_order().is_empty(), "cmd-N skips folded workspaces");
        });
        press(cx, &["l", "space", "s", "d"]);
        assert!(sections(cx).is_empty());
        cx.update(|cx| assert_eq!(shark.read(cx).state.workspaces.len(), 1));
        press(cx, &["ctrl-w", "h", "space", "w", "d"]);
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            shark.read(cx).state.workspaces.is_empty()
        })
        .await;

        // Quick bars are bare menus: no dialog close X.
        press(cx, &["cmd-k"]);
        cx.run_until_parked();
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            assert!(
                w.within("dialog").try_find("close").is_none(),
                "palette shows a close X"
            );
        })
        .unwrap();
        fs::remove_dir_all(&home).unwrap();
    }

    /// Diff states, `v` in the diff, refreshes that keep the file, New conversation, archive.
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn diff_states_and_archive(cx: &mut TestAppContext) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = test_home();
        let repo = home.join("src/api");
        init_repo(&repo);
        let (window, shark) = boot(
            cx,
            State {
                repos: vec![Repo {
                    name: "api".into(),
                    path: repo,
                }],
                ..Default::default()
            },
        );
        let repos = cx.update(|cx| shark.read(cx).state.repos.clone());
        let on = on_app(window, shark.clone());
        on(cx, &|s, w, cx| {
            s.create_workspace("st".into(), repos.clone(), None, w, cx)
        });
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            shark.read(cx).active.is_some()
        })
        .await;
        on(cx, &|s, w, cx| s.show(Tab::Diff, w, cx));
        cx.update(|cx| {
            let s = shark.read(cx);
            assert!(s.diff_loaded && s.diff_error.is_none() && s.diff_files.is_empty());
            assert!(
                s.diff_placeholder(cx).is_some(),
                "no changes shows the empty state"
            );
        });

        let api = home.join("shark/workspaces/st/api");
        for f in ["a", "b", "c"] {
            fs::write(api.join(f), "x\n").unwrap();
        }
        on(cx, &|s, _, cx| s.refresh_diff(cx));
        cx.update(|cx| assert_eq!(shark.read(cx).diff_files.len(), 3));
        let press = |cx: &mut TestAppContext, k: &str| {
            cx.update_window(window, |_, w, cx| w.press(k, cx)).unwrap();
        };
        let cursor = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                shark
                    .read(cx)
                    .diff
                    .read(cx)
                    .selected_lines()
                    .map(|r| r.path().to_string())
            })
        };
        // A freshly loaded diff has a line cursor from the first `j`.
        press(cx, "j");
        assert_eq!(cursor(cx).as_deref(), Some("api/a"));
        press(cx, "}");
        press(cx, "v");
        let at = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                let s = shark.read(cx);
                let row = s.tree_rows[s.tree_sel].file;
                (
                    s.diff_files[s.diff_file].path().to_string(),
                    row == Some(s.diff_file),
                )
            })
        };
        assert_eq!(at(cx), ("api/b".into(), true));
        cx.update(|cx| assert_eq!(shark.read(cx).viewed(1), Viewed::Yes, "v in the diff"));
        // Another file changes: the diff and the tree stay on b.
        fs::write(api.join("a"), "y\n").unwrap();
        fs::write(api.join("0"), "new\n").unwrap();
        on(cx, &|s, _, cx| s.refresh_diff(cx));
        assert_eq!(at(cx), ("api/b".into(), true));
        // j walks a line cursor (past the collapsed, viewed b); a refresh keeps it and `c` targets it.
        press(cx, "j");
        press(cx, "j");
        assert_eq!(cursor(cx).as_deref(), Some("api/c"));
        fs::write(api.join("0"), "newer\n").unwrap();
        on(cx, &|s, _, cx| s.refresh_diff(cx));
        assert_eq!(
            cursor(cx).as_deref(),
            Some("api/c"),
            "cursor survives a refresh"
        );
        assert_eq!(at(cx).0, "api/c", "v/o follow the cursor");
        press(cx, "c");
        cx.update_window(window, |_, w, cx| w.input("rename this", cx))
            .unwrap();
        press(cx, "enter");
        cx.run_until_parked();
        cx.update(|cx| {
            let p = review::prompt(shark.read(cx).ws_comments());
            assert!(p.contains("\napi/c:1\n> rename this\n"), "{p}");
        });

        // A git failure is not "no changes".
        on(cx, &|s, _, cx| {
            s.state.workspaces[0].repos[0].base = "no/such-base".into();
            s.refresh_diff(cx)
        });
        cx.update(|cx| {
            let s = shark.read(cx);
            assert!(
                s.diff_error
                    .as_deref()
                    .is_some_and(|e| e.starts_with("api: "))
            );
            assert!(s.diff_placeholder(cx).is_some());
        });

        // New conversation forgets the session in state.json too.
        let chat = cx.update(|cx| shark.read(cx).chats["st"].clone());
        on(cx, &|s, _, cx| {
            s.state.workspaces[0].chats[0].session_id = Some("old".into());
            chat.update(cx, |c, _| c.meta.session_id = Some("old".into()));
        });
        let row = COMMANDS
            .iter()
            .position(|(l, ..)| l.starts_with("New conversation"))
            .unwrap();
        on(cx, &|s, w, cx| (COMMANDS[row].2)(s, w, cx));
        assert_eq!(State::load().workspaces[0].chats[0].session_id, None);

        // Archive runs in the background, once.
        on(cx, &|s, w, cx| {
            s.remove_workspace("st", true, w, cx);
            assert!(s.archiving.contains("st"));
            s.remove_workspace("st", true, w, cx);
        });
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            shark.read(cx).state.workspaces.is_empty()
        })
        .await;
        cx.update(|cx| assert!(shark.read(cx).archiving.is_empty()));
        assert!(!api.exists());
        fs::remove_dir_all(&home).ok();
    }

    /// A repo joins a running workspace; the open workspace and tab come back after a restart.
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn add_repo_and_restore(cx: &mut TestAppContext) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = test_home();
        let repos: Vec<Repo> = ["api", "web"]
            .map(|name| {
                let path = home.join("src").join(name);
                init_repo(&path);
                Repo {
                    name: name.into(),
                    path,
                }
            })
            .into();
        let (window, shark) = boot(
            cx,
            State {
                repos: repos.clone(),
                ..Default::default()
            },
        );
        let on = on_app(window, shark.clone());
        on(cx, &|s, w, cx| {
            s.create_workspace("grow".into(), repos[..1].to_vec(), None, w, cx)
        });
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            shark.read(cx).active.is_some()
        })
        .await;
        // A second tab hears about the repo too.
        on(cx, &|s, w, cx| {
            s.new_chat(w, cx);
            s.add_repo_to_workspace("grow".into(), repos[1].clone(), w, cx)
        });
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            shark.read(cx).state.workspaces[0].repos.len() == 2
        })
        .await;
        let ws_dir = home.join("shark/workspaces/grow");
        assert!(ws_dir.join("web").exists());
        assert_eq!(State::load().workspaces[0].repos.len(), 2, "saved");
        let chat = cx.update(|cx| shark.read(cx).chats["grow"].clone());
        cx.update(|cx| assert_eq!(chat.read(cx).ws.repos.len(), 2, "the chat knows"));
        let second = cx.update(|cx| shark.read(cx).chats["grow~2"].clone());
        cx.update(|cx| assert_eq!(second.read(cx).ws.repos.len(), 2, "so does the second tab"));
        on(cx, &|s, w, cx| s.select_chat(0, w, cx));
        on(cx, &|s, w, cx| s.show(Tab::Diff, w, cx));
        assert_eq!(
            State::load().last_open,
            Some(("grow".into(), "diff".into()))
        );

        // A restart reopens it on the Diff tab.
        let (window2, shark2) = open(cx);
        cx.run_until_parked();
        cx.update(|cx| {
            let s = shark2.read(cx);
            assert_eq!(s.active.as_deref(), Some("grow"));
            assert_eq!(s.tab, Tab::Diff);
        });
        cx.update_window(window2, |_, w, _| w.remove_window())
            .unwrap();

        // Sends are refused while the worktrees are being removed.
        on(cx, &|s, w, cx| {
            s.remove_workspace("grow", true, w, cx);
            assert!(chat.read(cx).archiving);
            // Also when it is first opened during the removal.
            s.chats.remove("grow");
            s.syncs.remove("grow");
            s.open_workspace_now(0, w, cx);
            assert!(s.chats["grow"].read(cx).archiving);
        });
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            shark.read(cx).state.workspaces.is_empty()
        })
        .await;
        assert!(!ws_dir.exists());
        fs::remove_dir_all(&home).ok();
    }

    /// Times the hot paths on a big change: 2000 changed files and a 50K-line file.
    /// Run in release for real numbers: `cargo test --release --features test-support perf_ -- --nocapture`.
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn perf_large_change(cx: &mut TestAppContext) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = test_home();
        let repo = home.join("src/big");
        init_repo(&repo);
        State {
            repos: vec![Repo {
                name: "big".into(),
                path: repo.clone(),
            }],
            workspaces: vec![],
            vim: true,
            ..Default::default()
        }
        .save();
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::bind_keys(cx);
        });
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(0.), px(0.)),
                size(px(1440.), px(900.)),
            ))),
            ..Default::default()
        };
        let (window, shark) = cx
            .update(|cx| gpui_kit::open_window(options, cx, |w, cx| cx.new(|cx| Shark::new(w, cx))))
            .unwrap();
        let repos = cx.update(|cx| shark.read(cx).state.repos.clone());
        cx.update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| {
                s.create_workspace("perf".into(), repos, None, w, cx)
            })
        })
        .unwrap();
        cx.wait_for(window, Duration::from_secs(20), |_, cx| {
            shark.read(cx).active.is_some()
        })
        .await;
        let ws_dir = home.join("shark/workspaces/perf/big");
        for i in 0..2000 {
            let dir = ws_dir.join(format!("src/mod{}/sub{}", i % 40, i % 7));
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join(format!("f{i}.rs")),
                format!("fn f{i}() {{}}\n").repeat(20),
            )
            .unwrap();
        }
        let big: String = (0..50_000usize)
            .map(|i| {
                format!(
                    "    let value_{i} = compute(value_{}, \"line {i}\"); // note\n",
                    i.saturating_sub(1)
                )
            })
            .collect();
        fs::write(ws_dir.join("big.rs"), &big).unwrap();

        let ms = |t: Instant| t.elapsed().as_secs_f64() * 1000.;
        let frame = |cx: &mut TestAppContext| {
            cx.update_window(window, |_, w, cx| w.render_frame(cx))
                .unwrap()
        };
        let press = |cx: &mut TestAppContext, k: &str| {
            cx.update_window(window, |_, w, cx| w.press(k, cx)).unwrap();
        };
        let mut report = vec![];

        let ws = cx.update(|cx| shark.read(cx).active_ws().unwrap().clone());
        let t = Instant::now();
        let data = DiffData::load(&ws);
        report.push(format!(
            "diff load (git + parse, background): {:.0} ms",
            ms(t)
        ));
        assert_eq!(data.files.len(), 2001);
        // Not `space d`: the new workspace's composer has focus and would take the keys.
        cx.update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| s.show(Tab::Diff, w, cx))
        })
        .unwrap();
        let t = Instant::now();
        cx.update(|cx| shark.update(cx, |s, cx| s.apply_diff(data, cx)));
        frame(cx);
        report.push(format!("apply diff + first frame: {:.0} ms", ms(t)));
        let t = Instant::now();
        let data = DiffData::load(&ws);
        cx.update(|cx| shark.update(cx, |s, cx| s.apply_diff(data, cx)));
        frame(cx);
        report.push(format!("unchanged refresh + frame: {:.0} ms", ms(t)));

        cx.update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| s.focus_pane(Pane::Tree, w, cx))
        })
        .unwrap();
        frame(cx);
        let n = 200;
        // The tree cursor starts on the first file's row, not on its folders.
        let start = cx.update(|cx| shark.read(cx).tree_sel);
        let t = Instant::now();
        for _ in 0..n {
            press(cx, "j");
            frame(cx);
        }
        let tree_key = ms(t) / n as f64;
        report.push(format!("tree j + frame: {tree_key:.2} ms/key"));
        cx.update(|cx| assert_eq!(shark.read(cx).tree_sel, start + n));
        let t = Instant::now();
        for _ in 0..20 {
            press(cx, "v");
            frame(cx);
        }
        report.push(format!("toggle viewed + frame: {:.2} ms", ms(t) / 20.));
        cx.update(|cx| shark.update(cx, |s, cx| s.tree_select(0, cx)));
        let t = Instant::now();
        press(cx, "v");
        frame(cx);
        report.push(format!("toggle folder viewed + frame: {:.0} ms", ms(t)));
        cx.update(|cx| assert_eq!(shark.read(cx).viewed_now.len(), 2001));
        let t = Instant::now();
        press(cx, "shift-g");
        press(cx, "k");
        press(cx, "h");
        frame(cx);
        press(cx, "l");
        frame(cx);
        report.push(format!("collapse + expand folder: {:.1} ms", ms(t)));

        // big.rs sorts after the folders: the last row.
        let last = cx.update(|cx| shark.read(cx).tree_rows.len() - 1);
        cx.update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| {
                s.tree_select(last, cx);
                s.open_file(s.tree_rows[last].file.unwrap(), w, cx)
            })
        })
        .unwrap();
        let editor = cx.update(|cx| shark.read(cx).editor.clone().unwrap());
        // Syntax highlighting of a big file runs on a background thread in the app; the test
        // executor runs it inline, so time only what the main thread does when a file opens.
        cx.wait_for(window, Duration::from_secs(60), |_, cx| {
            editor.read(cx).input.read(cx).text().len() == big.len()
        })
        .await;
        frame(cx);
        let t = Instant::now();
        cx.update_window(window, |_, w, cx| {
            editor.update(cx, |e, cx| {
                e.input.update(cx, |i, cx| i.set_value(big.clone(), w, cx))
            })
        })
        .unwrap();
        frame(cx);
        report.push(format!(
            "open 50K-line file, main thread (set_value + frame): {:.0} ms",
            ms(t)
        ));

        let t = Instant::now();
        (0..50).for_each(|_| frame(cx));
        report.push(format!(
            "editor idle frame (baseline): {:.2} ms",
            ms(t) / 50.
        ));
        let mut keys = |cx: &mut TestAppContext, label: &str, seq: &[&str], rounds: usize| {
            let t = Instant::now();
            for _ in 0..rounds {
                for k in seq {
                    press(cx, k);
                    frame(cx);
                }
            }
            let per = ms(t) / (rounds * seq.len()) as f64;
            report.push(format!("{label}: {per:.2} ms/key"));
            per
        };
        let motion = keys(cx, "vim j/w/e/b + frame", &["j", "w", "e", "b"], 50);
        keys(cx, "vim ctrl-d / ctrl-u + frame", &["ctrl-d", "ctrl-u"], 20);
        let edit = keys(cx, "vim dd / u + frame", &["d", "d", "u"], 10);
        press(cx, "shift-g");
        frame(cx);
        keys(cx, "vim G / gg + frame", &["g", "g", "shift-g"], 10);
        press(cx, "i");
        let typing = keys(
            cx,
            "insert-mode typing + frame",
            &["a", "b", "c", "space"],
            25,
        );
        press(cx, "escape");
        cx.update(|cx| assert!(editor.read(cx).dirty));
        // Baseline: the editor's own arrow key, so Vim's share of the cost is visible.
        let arrow = keys(
            cx,
            "editor's own down key + frame (baseline)",
            &["down"],
            100,
        );
        println!(
            "\n--- perf ({} build) ---",
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
        );
        report.iter().for_each(|l| println!("{l}"));
        // Budgets hold in debug too; release is several times faster.
        assert!(tree_key < 50., "tree navigation {tree_key:.1} ms/key");
        assert!(motion < 50., "vim motion {motion:.1} ms/key");
        assert!(edit < 80., "vim edit {edit:.1} ms/key");
        assert!(typing < 50., "typing {typing:.1} ms/key");
        assert!(
            motion < arrow * 1.5 + 1.,
            "vim adds {:.1} ms/key over the editor",
            motion - arrow
        );
        fs::remove_dir_all(&home).ok();
    }
}
