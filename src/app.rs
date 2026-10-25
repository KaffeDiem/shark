//! Root view: sidebar of repos and workspaces, Chat/Diff tabs, palette and dialogs.
use crate::attention::Attn;
use crate::chat::{Chat, ChatEvent, MODELS};
use crate::editor::{EditorEvent, FileEditor};
use crate::files::{self, Row};
use crate::menu::{self, Entry};
use crate::state::{ChatMeta, Repo, Section, State, Workspace, move_item, workspaces_dir};
use crate::sync::{GitSync, SyncEvent};
use crate::*;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::dialog::{Dialog, DialogEntrance};
use gpui_kit::component::diff::{Diff, DiffFile, DiffFileStatus, DiffMode, DiffState};
use gpui_kit::component::hover_card::HoverCard;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::link::Link;
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

mod chats;
mod diff;
mod editor_pane;
mod media;
mod menus;
mod palette;
mod picker;
mod review;
mod sidebar;
#[cfg(all(test, feature = "test-support"))]
pub(crate) mod ui_tests;
mod workspace;

use diff::Viewed;
use menus::{copy, on_shark, open_terminal, tilde, ws_menu};
use palette::menu_dialog;
pub(crate) use palette::open_menu;
use sidebar::{Naming, SideRow, move_submenu};

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

pub(crate) fn notify_err(window: &mut Window, cx: &mut App, e: impl Into<SharedString>) {
    window.push_notification(Notification::error(e), cx);
}

fn kbd(keys: &str) -> Kbd {
    Kbd::new(Keystroke::parse(keys).unwrap())
}

/// A Danger-button alert; `ok` runs on the entity once confirmed.
pub(crate) fn confirm<T: 'static>(
    title: String,
    description: &'static str,
    ok_text: &'static str,
    ok: impl Fn(&mut T, &mut Window, &mut Context<T>) + 'static,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let (target, ok) = (cx.entity().downgrade(), Rc::new(ok));
    window.open_alert_dialog(cx, move |d, _, _| {
        let (target, ok) = (target.clone(), ok.clone());
        d.font_features(inter_features())
            .title(title.clone())
            .description(description)
            .ok_text(ok_text)
            .ok_variant(ButtonVariant::Danger)
            .show_cancel(true)
            .on_ok(move |_, window, cx| {
                target.update(cx, |s, cx| ok(s, window, cx)).ok();
                true
            })
    });
}

/// A list row under the cursor gets `tint` (also on hover), plus an accent ring while the list
/// has focus: the tint alone is too subtle to find the cursor.
fn cursor_row<E: Styled + InteractiveElement>(
    e: E,
    selected: bool,
    focused: bool,
    tint: Hsla,
    theme: &Theme,
) -> E {
    let e = (e.border_1())
        .border_color(theme.transparent)
        .hover(|d| d.bg(tint));
    match (selected, focused) {
        (true, true) => e.bg(theme.list_active).border_color(theme.primary),
        (true, false) => e.bg(tint),
        (false, _) => e,
    }
}

/// A header tab: the Chat, Diff and file pills.
fn pill(id: impl Into<SharedString>, on: bool, theme: &Theme) -> Stateful<Div> {
    h_flex()
        .id(id.into())
        .h(px(26.))
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
    /// The chat's plan pane, right of its transcript.
    Plan,
    Diff,
    Editor,
    Tree,
}

pub struct Shark {
    pub(crate) state: State,
    sel: SideRow,
    naming: Option<Naming>,
    pub(crate) active: Option<String>,
    /// By chat id; created when a chat is first shown.
    chats: HashMap<String, Entity<Chat>>,
    /// `caffeinate` while agents work and `stay_awake` is on.
    awake: Option<std::process::Child>,
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
    /// The chat tab strip, which scrolls sideways once the tabs outgrow the header.
    tab_scroll: ScrollHandle,
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
    /// The before/after row of each image or video in the diff, by path.
    media: Vec<(SharedString, Entity<media::MediaView>)>,
    media_cache: Entity<media::Lru>,
    /// The diff pane's width: media rows fit it, not the diff's longest line.
    media_width: Rc<std::cell::Cell<Pixels>>,
    pub(crate) syncs: HashMap<String, Entity<GitSync>>,
    sidebar_focus: FocusHandle,
    pub(crate) attn: crate::attention::Attention,
    /// The open right-click menu, dropped when it dismisses.
    menu: Option<(Entity<menu::Menu>, Subscription)>,
    _subs: Vec<Subscription>,
}

impl Shark {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = State::load();
        crate::prompts::set_overrides(&state.prompts);
        crate::settings::apply(&state.appearance, cx);
        let sidebar_focus = cx.focus_handle();
        sidebar_focus.focus(window, cx);
        cx.defer_in(window, |s, window, cx| s.restore_last_open(window, cx));
        cx.background_executor()
            .spawn(async { media::prune_cache() })
            .detach();
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
            awake: None,
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
            tab_scroll: ScrollHandle::new(),
            archiving: HashSet::new(),
            show_tree: true,
            rail: false,
            rail_auto: false,
            editor: None,
            editor_header: Default::default(),
            _editor_subs: vec![],
            review: Default::default(),
            media: vec![],
            media_cache: media::Lru::new(cx),
            media_width: Rc::default(),
            syncs: HashMap::new(),
            sidebar_focus,
            attn: crate::attention::Attention::new(cx),
            menu: None,
            _subs,
        }
    }

    fn show_menu(
        &mut self,
        pos: Point<Pixels>,
        entries: Vec<Entry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let m = menu::open(entries, pos, window, cx);
        let sub = cx.subscribe(&m, |this, _, _: &DismissEvent, cx| {
            this.menu = None;
            cx.notify();
        });
        self.menu = Some((m, sub));
        cx.notify();
    }

    fn ws(&self, name: &str) -> Option<&Workspace> {
        self.state.workspaces.iter().find(|w| w.name == name)
    }

    fn ws_mut(&mut self, name: &str) -> Option<&mut Workspace> {
        self.state.workspaces.iter_mut().find(|w| w.name == name)
    }

    fn active_ws(&self) -> Option<&Workspace> {
        self.ws(self.active.as_ref()?)
    }

    fn active_ws_mut(&mut self) -> Option<&mut Workspace> {
        let name = self.active.clone()?;
        self.ws_mut(&name)
    }

    fn active_chat(&self) -> Option<&Entity<Chat>> {
        let ws = self.active_ws()?;
        self.chats.get(&ws.chats.get(ws.active_chat)?.id)
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
            None => notify_err(window, cx, "Open a workspace first"),
        }
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

    fn panes(&self, cx: &App) -> Vec<Pane> {
        let mut v = vec![Pane::Sidebar];
        match self.tab {
            Tab::Chat => {
                v.push(Pane::Chat);
                if self.active_chat().is_some_and(|c| c.read(cx).plan_open) {
                    v.push(Pane::Plan);
                }
            }
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
            if c.plan_focused(window, cx) {
                return Some(Pane::Plan);
            }
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
            Pane::Plan => {
                if let Some(chat) = self.active_chat() {
                    chat.read(cx).plan.read(cx).focus.clone().focus(window, cx);
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
        let panes = self.panes(cx);
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

    fn render_main(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let chat_tabs = self.render_chat_tabs(cx);
        let theme = cx.theme();
        let Some(chat) = self.active_chat().cloned() else {
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
                            vec![kbd("up"), kbd("down"), kbd("enter")],
                            "open from sidebar",
                        ))
                        .child(tip(vec![kbd("cmd-n")], "new workspace"))
                        .child(tip(vec![kbd("cmd-k")], "every command, like Add repo")),
                )
                .into_any_element();
        };
        let tab = |label: String, icon: IconName, t: Tab| {
            pill(label.clone(), self.tab == t, theme)
                .px_2p5()
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
                    }),
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
}

impl Render for Shark {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            // `Vim` turns on the Vim-style list keys (see `bind_keys`).
            .key_context(if self.state.vim { "Shark Vim" } else { "Shark" })
            .size_full()
            .bg(theme.sidebar)
            .text_color(theme.foreground)
            .font_features(inter_features())
            .on_action(cx.listener(|this, _: &TogglePalette, w, cx| this.toggle_palette(w, cx)))
            .on_action(cx.listener(|this, _: &NewWorkspace, w, cx| {
                this.new_workspace(this.state.last_repos.clone(), w, cx)
            }))
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
            .on_action(
                cx.listener(|this, _: &OpenPrs, w, cx| this.with_sync(w, cx, GitSync::open_prs)),
            )
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
            .on_action(cx.listener(|this, _: &TogglePlanMode, w, cx| {
                this.with_plan(w, cx, |c, _, cx| c.set_plan_mode(!c.meta.plan, cx))
            }))
            .on_action(cx.listener(|this, _: &TogglePlan, w, cx| {
                this.with_plan(w, cx, |c, w, cx| c.toggle_plan(w, cx))
            }))
            .on_action(cx.listener(|this, _: &ApprovePlan, w, cx| {
                this.with_plan(w, cx, |c, w, cx| c.approve_plan(w, cx))
            }))
            .on_action(cx.listener(|this, _: &BuildPlanInNewChat, w, cx| {
                this.build_plan_in_new_chat(w, cx)
            }))
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
                if let Some(ix) = this.ws_order(false).get(a.0).copied() {
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
            // On the root, not the editor, so File > Save works with focus in the tree.
            .on_action(cx.listener(|this, _: &SaveFile, w, cx| {
                if let Some(e) = this.editor.clone() {
                    e.update(cx, |e, cx| e.save(false, w, cx))
                }
            }))
            // `o` in the diff and the tree.
            .on_action(cx.listener(|this, _: &OpenFile, w, cx| {
                if let Some(ix) = this.cursor_file(w, cx) {
                    this.open_file(ix, w, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &OpenAnyFile, w, cx| this.open_any_file(w, cx)))
            .on_action(cx.listener(|this, _: &OpenSettings, w, cx| {
                crate::settings::open(&this.state, w, cx)
            }))
            .child(self.render_sidebar(window, cx))
            .children(self.menu.as_ref().map(|(m, _)| menu::layer(m.clone())))
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
