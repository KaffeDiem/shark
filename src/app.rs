//! Root view: sidebar of repos and workspaces, Chat/Diff tabs, palette and dialogs.
use crate::chat::{Chat, ChatEvent, MODELS};
use crate::editor::{EditorEvent, FileEditor};
use crate::files::{self, Row};
use crate::state::{ChatMeta, Repo, State, Workspace, workspaces_dir};
use crate::sync::{GitSync, SyncEvent};
use crate::*;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::diff::{Diff, DiffFile, DiffFileStatus, DiffMode, DiffState};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::kbd::Kbd;
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
        d.w(px(640.))
            .font_features(inter_features())
            .margin_top(px(96.))
            .entrance(DialogEntrance::None)
            .close_button(false)
            .child(
                Command::new(&state)
                    .items(items.clone())
                    .on_confirm(move |ix, window, cx| {
                        window.close_dialog(cx);
                        target
                            .update(cx, |s, cx| on_pick(s, ix.row, window, cx))
                            .ok();
                    }),
            )
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
}

impl DiffData {
    fn load(ws: &Workspace) -> DiffData {
        let patch = git::combined_diff(ws, &ws.dir());
        let fp = files::fingerprint(&patch);
        let mut parsed: Vec<(DiffFile, u64)> = files::split_patch(&patch)
            .into_iter()
            .filter_map(|chunk| {
                let file = DiffFile::parse(chunk).ok()?.into_iter().next()?;
                Some((file, files::fingerprint(chunk)))
            })
            .collect();
        let paths: Vec<String> = parsed.iter().map(|(f, _)| f.path().to_string()).collect();
        let order = files::tree_order(&paths);
        let mut slots: Vec<Option<(DiffFile, u64)>> = parsed.drain(..).map(Some).collect();
        let (files, fps) = order.into_iter().filter_map(|i| slots[i].take()).unzip();
        DiffData { fp, files, fps }
    }
}

pub struct Shark {
    pub(crate) state: State,
    sel: usize,
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
    diff_file: usize,
    /// Paths currently viewed (fingerprint still matches), for the diff headers.
    viewed_now: Rc<HashSet<SharedString>>,
    tree_rows: Vec<Row>,
    collapsed_dirs: HashSet<String>,
    tree_sel: usize,
    tree_focus: FocusHandle,
    tree_scroll: UniformListScrollHandle,
    show_tree: bool,
    /// Sidebar shrunk to icons; `rail_auto` when opening a file did it, so closing undoes it.
    rail: bool,
    rail_auto: bool,
    editor: Option<Entity<FileEditor>>,
    /// What the editor header shows, to redraw only when it changes, not on every key.
    editor_header: (bool, bool, &'static str, String),
    _editor_subs: Vec<Subscription>,
    pub(crate) syncs: HashMap<String, Entity<GitSync>>,
    pub(crate) sidebar_focus: FocusHandle,
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
    ("Save file", IconName::Save, |s, w, cx| {
        if let Some(e) = s.editor.clone() {
            e.update(cx, |e, cx| e.save(false, w, cx))
        }
    }),
    ("Close file", IconName::X, |s, w, cx| s.close_file(w, cx)),
    ("Toggle Vim keys", IconName::Keyboard, |s, w, cx| {
        s.toggle_vim(w, cx)
    }),
    ("Stop agent", IconName::Square, |s, _, cx| s.stop(cx)),
    (
        "Manage queued messages",
        IconName::ListOrdered,
        |s, w, cx| s.focus_queue(w, cx),
    ),
    ("Change model…", IconName::Sparkles, |s, w, cx| {
        s.pick_model(w, cx)
    }),
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
        let _subs = vec![cx.observe_window_appearance(window, |s, _, cx| {
            if s.state.appearance.mode == crate::state::Mode::System {
                crate::settings::apply(&s.state.appearance, cx)
            }
        })];
        Shark {
            state,
            sel: 0,
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
            viewed_now: Rc::default(),
            tree_rows: vec![],
            collapsed_dirs: HashSet::new(),
            tree_sel: 0,
            tree_focus: cx.focus_handle(),
            tree_scroll: UniformListScrollHandle::new(),
            show_tree: true,
            rail: false,
            rail_auto: false,
            editor: None,
            editor_header: Default::default(),
            _editor_subs: vec![],
            syncs: HashMap::new(),
            sidebar_focus,
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
        let chat = cx.new(|cx| Chat::new(ws.clone(), meta.clone(), window, cx));
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

    fn open_workspace(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
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
            self.tree_rows.clear();
            self.collapsed_dirs.clear();
            self.tree_sel = 0;
        }
        self.sel = ix;
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
        self.active = Some(ws.name);
        self.refresh_diff(cx);
        self.show(Tab::Chat, window, cx);
        self.select_chat(ws.active_chat, window, cx);
    }

    fn show(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = tab;
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
        // Turn ends and saves refresh often; an unchanged patch keeps scroll and expansion.
        if data.fp == self.diff_fp && !self.diff_files.is_empty() {
            return;
        }
        let was_viewed = self.viewed_now.clone();
        self.diff_fp = data.fp;
        self.diff_fps = data.fps;
        self.diff_file = 0; // set_files scrolls back to the top
        self.diff_files = data.files;
        self.rebuild_tree();
        self.recompute_viewed();
        let files = self.diff_files.clone();
        let viewed = self.viewed_now.clone();
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

    /// The file a row action applies to: the open file in the file view, else the tree cursor.
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
        // The diff can't say which file is on screen; the highlighted tree row is what the user sees.
        self.tree_rows.get(self.tree_sel)?.file
    }

    fn open_file(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(f), Some(ws)) = (self.diff_files.get(ix), self.active_ws()) else {
            return;
        };
        let path = f.path().to_string();
        if self
            .editor
            .as_ref()
            .is_some_and(|e| e.read(cx).path == path)
        {
            return self.show(Tab::File, window, cx);
        }
        if f.status() == DiffFileStatus::Deleted || f.is_binary() {
            return Self::notify_err(
                window,
                cx,
                format!("{path} is deleted or binary; it shows in the diff only"),
            );
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

    fn create_workspace(
        &mut self,
        branch: String,
        repos: Vec<Repo>,
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
                    s.state.workspaces.push(ws);
                    s.state.save();
                    s.open_workspace(s.state.workspaces.len() - 1, window, cx);
                }
                Err(e) => Self::notify_err(window, cx, e),
            })
            .ok();
        })
        .detach();
    }

    /// Row actions hit the workspace under the sidebar cursor when the sidebar is focused, else the open one.
    fn target(&self, window: &Window) -> Option<Workspace> {
        let name = match self.sidebar_focus.is_focused(window) {
            true => self.state.workspaces.get(self.sel)?.name.clone(),
            false => self.active.clone()?,
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
            return Self::notify_err(window, cx, "An agent is running; stop it first (ctrl-c)");
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
        match git::remove_workspace(&ws, &ws.dir(), force) {
            Ok(()) => {
                // A later workspace with the same name must not replay or resume these chats.
                for c in ws.chats.iter().chain(&ws.closed) {
                    self.chats.remove(&c.id);
                    self.unread.remove(&c.id);
                    let _ = std::fs::remove_file(crate::agent::transcript_path(&c.id));
                }
                self.syncs.remove(name);
                self.state.workspaces.retain(|w| w.name != name);
                self.state.save();
                if self.active.as_deref() == Some(name) {
                    self.active = None;
                    self.drop_editor();
                    self.tab = Tab::Chat;
                }
                self.sel = self.sel.min(self.state.workspaces.len().saturating_sub(1));
                window.push_notification(
                    Notification::success(format!("Archived {name} (branch kept)")),
                    cx,
                );
                self.sidebar_focus.focus(window, cx);
            }
            Err(e) => Self::notify_err(window, cx, format!("{e}\nUse space w D to force.")),
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
    fn running_chats(&self, ws: &str, cx: &App) -> usize {
        let Some(ws) = self.state.workspaces.iter().find(|w| w.name == ws) else {
            return 0;
        };
        (ws.chats.iter())
            .filter(|c| self.chats.get(&c.id).is_some_and(|c| c.read(cx).working()))
            .count()
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
        let workspaces = self.state.workspaces.iter().enumerate().map(|(ix, w)| {
            let running = self.running_chats(&w.name, cx);
            let busy = running > 0;
            let selected = ix == self.sel;
            let active = self.active.as_ref() == Some(&w.name);
            let ws = w.clone();
            let shark = shark.clone();
            row()
                .id(ix)
                .h(px(44.))
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
                        this.sel = ix;
                        cx.notify();
                    }),
                )
                .child(match busy {
                    _ if cmd_held && ix < 9 => {
                        Kbd::new(Keystroke::parse(&format!("cmd-{}", ix + 1)).unwrap())
                            .into_any_element()
                    }
                    true => Spinner::new()
                        .icon(IconName::LoaderCircle)
                        .color(theme.primary)
                        .with_size(px(14.))
                        .into_any_element(),
                    false => Icon::new(IconName::CircleDashed)
                        .with_size(px(14.))
                        .text_color(theme.muted_foreground)
                        .into_any_element(),
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
                                        match running {
                                            0 | 1 => repos,
                                            n => format!("{n} chats running · {repos}"),
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
        });
        v_flex()
            .id("sidebar")
            .key_context("Sidebar")
            .track_focus(&self.sidebar_focus)
            .w(px(if rail { 48. } else { 244. }))
            .h_full()
            .flex_none()
            .text_color(theme.sidebar_foreground)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                this.sel = (this.sel + 1).min(this.state.workspaces.len().saturating_sub(1));
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| {
                this.sel = this.sel.saturating_sub(1);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| {
                this.sel = 0;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| {
                this.sel = this.state.workspaces.len().saturating_sub(1);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| {
                this.open_workspace(this.sel, window, cx)
            }))
            // Room for the traffic lights; the transparent titlebar lets the sidebar run under them.
            .child(
                div()
                    .h(px(36.))
                    .flex_none()
                    .window_control_area(WindowControlArea::Drag),
            )
            .child(header("Workspaces"))
            .children(workspaces)
            .when(self.state.workspaces.is_empty(), |d| {
                d.child(hint("space w n to create"))
            })
            .child(header("Repos"))
            .children(repos)
            .when(self.state.repos.is_empty(), |d| {
                d.child(hint("space r a to add"))
            })
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
        let (busy, background) = (chat.read(cx).working(), chat.read(cx).background);
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
        let ws_name = self.active.clone().unwrap_or_default();
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
                    .child(
                        h_flex()
                            .gap_1p5()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(Icon::new(IconName::GitBranch).with_size(px(12.)))
                            .child(ws_name),
                    )
                    .when_some(self.active_sync().cloned(), |d, sync| {
                        d.child(div().pl_3().child(sync))
                    })
                    .child(
                        h_flex()
                            .pl_3()
                            .gap_1p5()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(div().size(px(6.)).rounded_full().bg(if busy {
                                theme.primary
                            } else {
                                faint(theme)
                            }))
                            .child(match (chat.read(cx).busy, background) {
                                (true, 0) => "Running".into(),
                                (true, n) => format!("Running · {n} in background"),
                                (false, 0) => "Idle".into(),
                                (false, n) => format!("{n} in background"),
                            }),
                    ),
            )
            .child(match self.tab {
                Tab::Chat => div().flex_1().min_h_0().child(chat).into_any_element(),
                Tab::File => h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        h_resizable("chat-editor")
                            .child(
                                resizable_panel()
                                    .size(px(420.))
                                    .size_range(px(320.)..px(900.))
                                    .child(chat),
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

    fn render_diff(&self, cx: &mut Context<Self>) -> AnyElement {
        let viewed = self.viewed_now.clone();
        let shark = cx.entity().downgrade();
        div()
            .key_context("DiffPane")
            .flex_1()
            .min_w_0()
            .h_full()
            .on_action(cx.listener(|this, _: &NextHunk, _, cx| {
                this.diff.update(cx, |d, cx| d.next_change(cx))
            }))
            .on_action(cx.listener(|this, _: &PrevHunk, _, cx| {
                this.diff.update(cx, |d, cx| d.previous_change(cx))
            }))
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
            .child(
                Diff::new(&self.diff).render_header_prefix(move |file, _, _| {
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
        v_flex()
            .key_context("FileEditor")
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
            .on_action(
                cx.listener(|this, a: &OpenWorkspace, w, cx| this.open_workspace(a.0, w, cx)),
            )
            // The sidebar swaps status icons for cmd-N hints while cmd is held.
            .on_modifiers_changed(cx.listener(|_, _, _, cx| cx.notify()))
            .on_action(cx.listener(|this, _: &FocusLeft, w, cx| this.move_focus(-1, w, cx)))
            .on_action(cx.listener(|this, _: &FocusRight, w, cx| this.move_focus(1, w, cx)))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            .on_action(cx.listener(|this, _: &ToggleFileTree, w, cx| this.toggle_tree(w, cx)))
            .on_action(cx.listener(|this, _: &ToggleVim, w, cx| this.toggle_vim(w, cx)))
            .on_action(cx.listener(|this, _: &CloseFile, w, cx| this.close_file(w, cx)))
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
        self.shark
            .update(cx, |s, cx| s.create_workspace(branch, repos, window, cx))
            .ok();
        true
    }
}

impl Render for NewWorkspaceForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, faint) = (theme.muted_foreground, faint(theme));
        let in_branch = self.branch.read(cx).focus_handle(cx).is_focused(window);
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
                match this.branch.read(cx).focus_handle(cx).is_focused(window) {
                    true => this.picker.update(cx, |p, cx| p.focus(window, cx)),
                    false => this.branch.update(cx, |i, cx| i.focus(window, cx)),
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
mod ui_tests {
    use super::*;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin
    use gpui_kit::test::{TestAppContextExt, TestWindowExt};
    use std::sync::Mutex;
    use std::time::Instant;
    use std::{fs, path::PathBuf, time::Duration};

    /// UI tests share the process-wide `TEST_HOME` and `state.json`, so they take turns.
    static UI_LOCK: Mutex<()> = Mutex::new(());

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

    /// Settings apply and persist at once; an edited prompt reaches the git button, reset drops it.
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn settings_apply_and_save(cx: &mut TestAppContext) {
        use crate::settings::Settings;
        use crate::state::Mode;
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        test_home();
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::bind_keys(cx);
        });
        let (window, shark) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
                    cx.new(|cx| Shark::new(w, cx))
                })
            })
            .unwrap();
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
        State {
            repos,
            workspaces: vec![],
            vim: false,
            last_repos: vec!["api".into()],
            ..Default::default()
        }
        .save();

        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::bind_keys(cx);
        });
        let (window, shark) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
                    cx.new(|cx| Shark::new(w, cx))
                })
            })
            .unwrap();
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
        cx.update(|cx| assert!(shark.read(cx).state.workspaces.is_empty()));
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
        press(cx, &["ctrl-w", "h", "space", "w", "d"]);
        cx.update(|cx| assert!(shark.read(cx).state.workspaces.is_empty()));

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
            shark.update(cx, |s, cx| s.create_workspace("perf".into(), repos, w, cx))
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
        let t = Instant::now();
        for _ in 0..n {
            press(cx, "j");
            frame(cx);
        }
        let tree_key = ms(t) / n as f64;
        report.push(format!("tree j + frame: {tree_key:.2} ms/key"));
        cx.update(|cx| assert_eq!(shark.read(cx).tree_sel, n));
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
