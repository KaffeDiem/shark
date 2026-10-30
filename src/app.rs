//! Root view: sidebar of repos and workspaces, Chat/Diff tabs, palette and dialogs.
use crate::attention::Attn;
use crate::chat::{Chat, ChatEvent, MODELS};
use crate::checkbox::Checkbox;
use crate::editor::{EditorEvent, FileEditor};
use crate::menu::{self, Entry};
use crate::state::{ChatMeta, Repo, Section, State, TabId, Workspace, move_item, workspaces_dir};
use crate::sync::{GitSync, SyncEvent};
use crate::*;
use gpui_kit::assets::IconName;
use gpui_kit::base::InteractiveElementExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::dialog::{Dialog, DialogEntrance};
use gpui_kit::component::hover_card::HoverCard;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::link::Link;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{
    ResizablePanelEvent, ResizableState, h_resizable, resizable_panel,
};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme, Icon, IndexPath, Sizable as _, Theme, WindowExt, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

mod app_switcher;
mod chats;
mod differ_host;
mod editor_pane;
mod features;
mod file_changes;
mod home;
mod interaction;
pub(crate) mod menus;
mod palette;
mod picker;
mod reorder;
mod repo_settings;
mod run;
mod sidebar;
mod storage;
#[cfg(all(test, feature = "test-support"))]
pub(crate) mod ui_tests;
mod usage;
mod workspace;
mod workspace_picker;

use crate::features::{Feature, FeatureFocus, differ, git as git_feature};
use crate::ui::*;
use differ::Differ;
use differ::Viewed;
use menus::{on_shark, open_in_entries, tilde, ws_menu};
use palette::menu_dialog;
pub(crate) use palette::open_menu;
use sidebar::{Naming, SideRow, move_submenu};

#[derive(PartialEq, Clone, Copy, Debug)]
enum Tab {
    Chat,
    Feature(Feature),
    /// The full-width file editor; only while `editor` is set.
    File,
}

/// Focusable columns, left to right; `ctrl-w h/l` walks them.
#[derive(PartialEq, Clone, Copy, Debug)]
enum Pane {
    Sidebar,
    Chat,
    /// The chat's plan pane, right of its transcript.
    Plan,
    Feature(Feature, FeatureFocus),
    Editor,
    Features,
    Home,
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
    differ: Entity<Differ>,
    /// The sidebar | rest split, while the sidebar isn't a rail; persisted like Files.
    sidebar_resize: Entity<ResizableState>,
    sidebar_scroll: ScrollHandle,
    tab_scroll: ScrollHandle,
    reorder: Rc<RefCell<reorder::Reorder>>,
    tab_layout: Rc<std::cell::Cell<(Pixels, usize)>>,
    restore_generation: u64,
    chrome_pressed: Rc<std::cell::Cell<Option<&'static str>>>,
    /// Workspaces whose worktrees are being removed in the background.
    pub(crate) archiving: HashSet<String>,
    creating: HashSet<String>,
    adding_repos: HashSet<String>,
    /// Sidebar shrunk to icons by the user.
    rail: bool,
    /// Zen mode (cmd-.): only the shown tab's content; see `zen_shown`.
    zen: bool,
    editor: Option<Entity<FileEditor>>,
    file_tabs: Vec<editor_pane::FileTab>,
    pub(crate) syncs: HashMap<String, Entity<GitSync>>,
    sidebar_focus: FocusHandle,
    features_focus: FocusHandle,
    feature_sel: usize,
    git_features: HashMap<String, Entity<git_feature::GitFeature>>,
    pub(crate) attn: crate::attention::Attention,
    /// The open right-click menu, dropped when it dismisses.
    menu: Option<(Entity<menu::Menu>, Subscription)>,
    app_switcher: Option<app_switcher::AppSwitcher>,
    usage_panel: Option<usage::Panel>,
    usage: [crate::usage::Snapshot; 2],
    /// Daily tokens from the agents' local logs, rescanned with each usage refresh.
    usage_activity: [crate::usage::activity::Daily; 2],
    activity_cache: std::sync::Arc<[std::sync::Mutex<crate::usage::activity::Cache>; 2]>,
    #[cfg(test)]
    usage_queries: [usize; 2],
    #[cfg(test)]
    usage_live: bool,
    usage_at: Rc<std::cell::Cell<Bounds<Pixels>>>,
    usage_poll: Option<Task<()>>,
    usage_due: Option<std::time::Instant>,
    content_focus: FocusHandle,
    interaction_save: Option<Task<()>>,
    /// The header's branch button as last painted: "Branch menu…" hangs its menu from it.
    branch_at: Rc<std::cell::Cell<Bounds<Pixels>>>,
    /// The latest dependency check; empty until the first one lands.
    pub(crate) deps: Vec<crate::deps::Dep>,
    deps_check: Option<Task<()>>,
    /// When the last dependency check started.
    deps_at: Option<std::time::Instant>,
    pub(crate) home: home::Home,
    /// Running run scripts by workspace (cmd-r); dropping one stops them.
    runs: HashMap<String, crate::scripts::Running>,
    _subs: Vec<Subscription>,
}

impl Shark {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut state = State::load();
        crate::prompts::set_overrides(&state.prompts);
        crate::settings::apply(&state.appearance, cx);
        let sidebar_focus = cx.focus_handle();
        sidebar_focus.focus(window, cx);
        let backup = state.corrupt_backup.take();
        let read_only = state.read_only;
        // A fresh install has nothing new to announce (Home's What's new line); an update
        // from before the line existed has workspaces, and does.
        if state.last_seen_version.is_empty() && state.workspaces.is_empty() {
            state.last_seen_version = env!("CARGO_PKG_VERSION").into();
            let _ = state.save();
        }
        let home = home::Home::new(window, cx);
        let differ = cx.new(|cx| Differ::new(differ::Preferences::from(&state), window, cx));
        cx.defer_in(window, move |s, window, cx| {
            s.restore_last_open(window, cx);
            s.check_deps(cx);
            s.usage_polling(window, cx);
            let msg = match (backup, read_only) {
                (Some(bak), _) => Some(format!(
                    "Couldn't read state.json; moved it to {}",
                    bak.display()
                )),
                (None, true) => Some(
                    "Couldn't read state.json or move it aside; nothing is saved until you fix it and restart Shark".into(),
                ),
                _ => None,
            };
            if let Some(msg) = msg {
                let n = Notification::warning(msg).title("Saved state was reset");
                window.push_notification(n.autohide(false), cx);
            }
        });
        let sidebar_resize = cx.new(|_| ResizableState::default());
        // The close button asks before unsaved edits go, as Quit does.
        let this = cx.entity().downgrade();
        window.on_window_should_close(cx, move |w, cx| {
            (this.update(cx, |s, cx| s.close_window(w, cx))).unwrap_or(true)
        });
        let activity_owner = cx.entity().downgrade();
        let mut _subs = vec![
            cx.intercept_keystrokes(move |ev, w, cx| {
                activity_owner
                    .update(cx, |s, cx| s.interaction_key(&ev.keystroke, w, cx))
                    .ok();
            }),
            cx.subscribe(&sidebar_resize, |s, r, _: &ResizablePanelEvent, cx| {
                if let Some(w) = r.read(cx).sizes().first() {
                    s.state.sidebar_width = Some(w.as_f32());
                    let _ = s.state.save();
                }
            }),
            // Keys need a rendered target. A closing dialog gives focus back to what had it
            // when it opened, which may be gone by then: a closed select's popover, or Home
            // after what the dialog did (open a workspace) took Home away.
            cx.on_focus_lost(window, |s, w, cx| s.focus_fallback(w, cx)),
            cx.observe_window_appearance(window, |s, _, cx| {
                if s.state.appearance.mode == crate::state::Mode::System {
                    crate::settings::apply(&s.state.appearance, cx)
                }
            }),
            cx.observe_window_activation(window, |s, w, cx| {
                s.attn.window_active = w.is_window_active();
                s.usage_polling(w, cx);
                if !s.attn.window_active {
                    s.close_app_switcher(false, w, cx);
                }
                // Back from installing something: check again, but only if anything was missing,
                // and at most once a minute (the check runs a login shell).
                let due = s.deps_at.is_none_or(|t| t.elapsed().as_secs() >= 60);
                if s.attn.window_active && due && crate::deps::fixable(&s.deps) {
                    s.check_deps(cx)
                }
            }),
        ];
        // Dev servers started by cmd-r don't outlive Shark.
        _subs.push(cx.on_app_quit(|s, _| {
            crate::scripts::Running::stop_all(s.runs.drain().map(|(_, r)| r), 2);
            async {}
        }));
        _subs.push(cx.observe(&differ, |_, _, cx| cx.notify()));
        _subs.push(cx.subscribe_in(&differ, window, |s, _, event, w, cx| {
            s.differ_event(event, w, cx)
        }));
        Shark {
            state,
            sel: SideRow::Ws(0),
            naming: None,
            active: None,
            chats: HashMap::new(),
            awake: None,
            unread: HashSet::new(),
            tab: Tab::Chat,
            differ,
            sidebar_resize,
            sidebar_scroll: ScrollHandle::new(),
            tab_scroll: ScrollHandle::new(),
            reorder: Rc::default(),
            tab_layout: Rc::default(),
            restore_generation: 0,
            chrome_pressed: Rc::default(),
            archiving: HashSet::new(),
            creating: HashSet::new(),
            adding_repos: HashSet::new(),
            rail: false,
            zen: false,
            editor: None,
            file_tabs: vec![],
            syncs: HashMap::new(),
            sidebar_focus,
            features_focus: cx.focus_handle(),
            feature_sel: 0,
            git_features: HashMap::new(),
            attn: crate::attention::Attention::new(cx),
            menu: None,
            app_switcher: None,
            usage_panel: None,
            usage: Default::default(),
            usage_activity: Default::default(),
            activity_cache: Default::default(),
            #[cfg(test)]
            usage_queries: [0; 2],
            #[cfg(test)]
            usage_live: false,
            usage_at: Default::default(),
            usage_poll: None,
            usage_due: None,
            content_focus: cx.focus_handle(),
            interaction_save: None,
            branch_at: Default::default(),
            deps: vec![],
            deps_check: None,
            deps_at: None,
            home,
            runs: HashMap::new(),
            _subs,
        }
    }

    /// Runs `deps::check` in the background and stores the result.
    fn check_deps(&mut self, cx: &mut Context<Self>) {
        if cfg!(test) || self.deps_check.is_some() {
            return; // the UI tests don't spawn gh; one check at a time
        }
        self.deps_at = Some(std::time::Instant::now());
        self.deps_check = Some(cx.spawn(async move |this, cx| {
            let deps = (cx.background_executor())
                .spawn(async { crate::deps::check() })
                .await;
            this.update(cx, |s, cx| {
                s.deps = deps;
                s.deps_check = None;
                cx.notify()
            })
            .ok();
        }));
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

    /// "Branch menu…": the header's branch menu, hanging from the button, from the keyboard.
    fn open_branch_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.active_ws() else {
            return notify_err(window, cx, "Open a workspace first");
        };
        let entries = menus::branch_entries(self, ws, cx);
        let b = self.branch_at.get();
        let m = menu::open_at(
            entries,
            point(b.right(), b.bottom() + px(4.)),
            Anchor::TopRight,
            window,
            cx,
        );
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
        if let Tab::Feature(feature) = tab {
            if self.active_ws().is_none() {
                return notify_err(window, cx, "Open a workspace first");
            }
            let ws = self.active_ws_mut().unwrap();
            let closed = match feature {
                Feature::Diff => &mut ws.diff_closed,
                Feature::Git => &mut ws.git_closed,
            };
            if *closed {
                *closed = false;
                ws.open_tab(feature.tab_id());
                let _ = self.state.save();
            }
            self.feature_sel = Feature::ALL.iter().position(|f| *f == feature).unwrap();
        }
        self.tab = tab;
        self.home.on = false;
        self.remember_open();
        self.reveal_tab(cx);
        self.configure_differ(cx);
        match tab {
            Tab::Chat => {
                if let Some(chat) = self.active_chat() {
                    let focus = chat.read(cx).focus.clone();
                    focus.focus(window, cx);
                }
            }
            Tab::Feature(feature) => self.activate_feature(feature, window, cx),
            Tab::File => match &self.editor {
                Some(e) => crate::editor::focus(e, window, cx),
                None => return self.show(self.file_fallback(), window, cx),
            },
        }
        cx.notify();
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        // In Zen the sidebar is hidden; cmd-b brings it back by leaving Zen.
        if self.zen_shown() {
            self.zen = false;
            return cx.notify();
        }
        self.rail = !self.rail;
        cx.notify();
    }

    /// Zen hides the sidebar, tabs, header and Features rail around the shown chat, Diff, Git
    /// or file; Home ends it.
    fn zen_shown(&self) -> bool {
        self.zen && !self.home_shown()
    }

    fn toggle_zen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.zen_shown() {
            self.zen = false;
            return cx.notify();
        }
        if self.home_shown() {
            return notify_err(window, cx, "Open a workspace first");
        }
        // Out of the hidden sidebar or rail, into the content; a field already in the content
        // (a side question) keeps the focus.
        let kept = self.content_focus.contains_focused(window, cx)
            && !self.features_focus.contains_focused(window, cx);
        self.zen = true;
        if !kept && let Some(&pane) = self.panes(cx).first() {
            self.focus_pane(pane, window, cx);
        }
        cx.notify();
    }

    fn panes(&self, cx: &App) -> Vec<Pane> {
        let mut v = match self.zen_shown() {
            true => vec![],
            false => vec![Pane::Sidebar],
        };
        if self.home_shown() {
            v.push(Pane::Home);
            return v;
        }
        match self.tab {
            Tab::Chat => {
                v.push(Pane::Chat);
                if self.active_chat().is_some_and(|c| c.read(cx).plan_open) {
                    v.push(Pane::Plan);
                }
            }
            Tab::Feature(feature) => v.extend(
                self.feature_focus_targets(feature, cx)
                    .into_iter()
                    .map(|target| Pane::Feature(feature, target)),
            ),
            Tab::File => v.push(Pane::Editor),
        }
        if self.right_rail_shown() {
            v.push(Pane::Features);
        }
        v
    }

    fn focused_pane(&self, window: &Window, cx: &App) -> Option<Pane> {
        if self.sidebar_focus.is_focused(window) {
            return Some(Pane::Sidebar);
        }
        if self.right_rail_shown() && self.features_focus.is_focused(window) {
            return Some(Pane::Features);
        }
        if self.home_shown() {
            return self.home.has_focus(window, cx).then_some(Pane::Home);
        }
        if let Tab::Feature(feature) = self.tab {
            return self
                .focused_feature(feature, window, cx)
                .map(|target| Pane::Feature(feature, target));
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
        None
    }

    fn focus_pane(&mut self, pane: Pane, window: &mut Window, cx: &mut Context<Self>) {
        match pane {
            Pane::Sidebar => self.sidebar_focus.focus(window, cx),
            Pane::Features => self.features_focus.focus(window, cx),
            Pane::Home => self.home.focus.focus(window, cx),
            Pane::Feature(feature, target) => self.focus_feature(feature, target, window, cx),
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
        if panes[to] != Pane::Chat || self.active.is_some() {
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

    fn render_sidebar_toggle(&self, cx: &Context<Self>) -> Button {
        side_toggle(
            "toggle-sidebar",
            if self.rail {
                IconName::PanelLeftOpen
            } else {
                IconName::PanelLeftClose
            },
            "Toggle sidebar (cmd-b)",
        )
        .text_color(cx.theme().sidebar_foreground)
        .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx)))
    }

    fn render_commands(&self, cx: &Context<Self>) -> Button {
        side_toggle("commands", IconName::Command, "Commands (cmd-k)")
            .text_color(cx.theme().sidebar_foreground)
            .on_click(cx.listener(|this, _, window, cx| this.toggle_palette(window, cx)))
    }

    fn render_new_workspace(&self, cx: &Context<Self>) -> Button {
        side_toggle(
            "new-workspace",
            IconName::SquarePen,
            "New workspace (cmd-n)",
        )
        .text_color(cx.theme().sidebar_foreground)
        .on_click(cx.listener(|this, _, window, cx| {
            this.new_workspace(this.state.last_repos.clone(), None, window, cx)
        }))
    }

    fn draggable_chrome(&self, id: &'static str, d: Stateful<Div>) -> impl IntoElement {
        let pressed = self.chrome_pressed.clone();
        d.window_control_area(WindowControlArea::Drag)
            .on_mouse_down_out({
                let pressed = pressed.clone();
                move |_, _, _| {
                    if pressed.get() == Some(id) {
                        pressed.set(None);
                    }
                }
            })
            .on_mouse_down(MouseButton::Left, {
                let pressed = pressed.clone();
                move |_, _, _| pressed.set(Some(id))
            })
            .on_mouse_up(MouseButton::Left, {
                let pressed = pressed.clone();
                move |_, _, _| {
                    if pressed.get() == Some(id) {
                        pressed.set(None);
                    }
                }
            })
            .on_mouse_up_out(MouseButton::Left, {
                let pressed = pressed.clone();
                move |_, _, _| {
                    if pressed.get() == Some(id) {
                        pressed.set(None);
                    }
                }
            })
            .on_mouse_move(move |ev, window, _| {
                if ev.pressed_button == Some(MouseButton::Left) && pressed.get() == Some(id) {
                    pressed.set(None);
                    window.start_window_move();
                }
            })
            .on_double_click(|_, window, _| window.titlebar_double_click())
            .test_support()
    }

    fn render_titlebar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = if self.home_shown() {
            vec![]
        } else {
            self.render_tabs(window, cx)
        };
        h_flex()
            .id("workspace-titlebar")
            .h(px(44.))
            .flex_none()
            .pl(px(8.))
            .when(!self.right_rail_shown(), |d| d.pr(px(8.)))
            .child(
                h_flex()
                    .min_w_0()
                    .gap(px(4.))
                    .when(self.rail, |d| {
                        d.child(self.render_sidebar_toggle(cx))
                            .child(self.render_commands(cx))
                            .child(self.render_new_workspace(cx))
                    })
                    .children(tabs)
                    .when(!self.home_shown() && self.active_ws().is_some(), |d| {
                        d.child(
                            Button::new("new-chat")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Plus)
                                .w(px(24.))
                                .flex_none()
                                .tooltip("New chat (cmd-t)")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.new_chat(window, cx)),
                                ),
                        )
                    }),
            )
            // Native dragging belongs only to unused space, so tabs keep their own drags.
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .id("titlebar-empty")
                    .map(|d| self.draggable_chrome("titlebar-empty", d)),
            )
            .test_support()
    }

    fn render_main(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        if self.home_shown() {
            return self.render_home(window, cx);
        }
        let controls = self.render_feature_controls(cx);
        let has_controls = controls.is_some();
        let theme = cx.theme();
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                h_flex()
                    .h(px(44.))
                    .flex_none()
                    .px(px(16.))
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex_shrink_1()
                            .min_w(px(28.))
                            .overflow_hidden()
                            .child(self.render_branch(cx)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .id("workspace-toolbar-empty")
                            .map(|d| self.draggable_chrome("workspace-toolbar-empty", d)),
                    )
                    .children(controls)
                    .when_some(self.active_sync().cloned(), |d, sync| {
                        d.child(
                            div()
                                .min_w_0()
                                .flex_shrink_0()
                                .when(has_controls, |d| d.max_w(relative(0.4)))
                                .child(sync),
                        )
                    }),
            )
            .child(self.render_tab(cx))
            .into_any_element()
    }

    /// The shown tab's content, below the header (or Zen's bar).
    fn render_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.tab {
            // Cached: the sidebar spinner redraws this view every frame, the chat only on change.
            Tab::Chat => div()
                .flex_1()
                .min_h_0()
                .children(
                    self.active_chat()
                        .map(|chat| chat.clone().cached(StyleRefinement::default().size_full())),
                )
                .into_any_element(),
            Tab::Feature(feature) => self.render_feature(feature, cx),
            Tab::File => h_flex()
                .flex_1()
                .min_h_0()
                .child(self.render_editor(cx))
                .into_any_element(),
        }
    }

    /// The header's branch label, `base ← branch`, with a stack icon and the branch's place in
    /// its stack (`2/3`) when it is part of one; hovering lists each repo's branch, linked to
    /// GitHub once pushed, and the base it targets. Either click opens the branch menu.
    fn render_branch(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let branch = self
            .active_ws()
            .map(|w| w.branch.clone())
            .unwrap_or_default();
        let links = self
            .active_sync()
            .map(|s| s.read(cx).branches())
            .unwrap_or_default();
        let stacks = (self.active_ws())
            .map(|w| menus::stacks(self, w, cx))
            .unwrap_or_default();
        let label = (self.active_ws())
            .map(|w| menus::branch_label(self, w, cx))
            .unwrap_or_default();
        let place = |s: &menus::RepoStack| format!("{}/{}", s.me + 1, s.nodes.len());
        let position = stacks.first().map(place);
        let per_repo: Vec<(String, String)> =
            stacks.iter().map(|s| (s.repo.clone(), place(s))).collect();
        HoverCard::new("branch-card")
            .anchor(Anchor::TopRight)
            .trigger(
                Button::new("branch")
                    .ghost()
                    .xsmall()
                    .w_full()
                    .h(px(28.))
                    .min_w_0()
                    .gap_1p5()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .accessibility_label(label.clone())
                    .child(
                        Icon::new(match position {
                            Some(_) => IconName::Layers,
                            None => IconName::GitBranch,
                        })
                        .with_size(px(12.))
                        .flex_none(),
                    )
                    .child(div().min_w_0().truncate().child(label))
                    .children(position.map(|p| div().flex_shrink_0().child(p)))
                    .child({
                        let at = self.branch_at.clone();
                        canvas(move |b, _, _| at.set(b), |_, _, _, _| {})
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                    })
                    .on_any_mouse_down(cx.listener(|this, e: &MouseDownEvent, w, cx| {
                        if !matches!(e.button, MouseButton::Left | MouseButton::Right) {
                            return;
                        }
                        let Some(ws) = this.active_ws() else { return };
                        let entries = menus::branch_entries(this, ws, cx);
                        this.show_menu(e.position, entries, w, cx);
                    })),
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
                    .children(links.iter().enumerate().map(|(ix, b)| {
                        h_flex()
                            .gap_1p5()
                            .child(Icon::new(IconName::FolderGit2).with_size(px(12.)))
                            .child(match &b.url {
                                Some(u) => Link::new(("branch-link", ix))
                                    .href(u.clone())
                                    .child(b.repo.clone())
                                    .into_any_element(),
                                None => h_flex()
                                    .gap_1p5()
                                    .child(b.repo.clone())
                                    .child(
                                        div()
                                            .text_color(theme.muted_foreground)
                                            .child("not on GitHub"),
                                    )
                                    .into_any_element(),
                            })
                            .child(div().text_color(theme.muted_foreground).child(
                                match per_repo.iter().find(|(r, _)| *r == b.repo) {
                                    Some((_, at)) => {
                                        format!("{} → {} · stack {at}", b.name, b.base)
                                    }
                                    None => format!("{} → {}", b.name, b.base),
                                },
                            ))
                    }))
            })
    }
    /// Zen (cmd-.): the shown tab alone on the content background. Its bar keeps the traffic
    /// lights clear, drags the window and holds the tab's own controls (Diff's).
    fn render_zen(&self, cx: &mut Context<Self>) -> Div {
        let controls = self.render_feature_controls(cx);
        let zen = v_flex()
            .size_full()
            .child(
                h_flex()
                    .h(px(44.))
                    .flex_none()
                    .pl(px(80.))
                    .pr(px(16.))
                    .when(controls.is_some(), |d| {
                        d.border_b_1().border_color(cx.theme().border)
                    })
                    .child(
                        div()
                            .id("zen-titlebar")
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .map(|d| self.draggable_chrome("zen-titlebar", d)),
                    )
                    .children(controls),
            )
            .children(self.render_unsaved(cx))
            .child(v_flex().flex_1().min_h_0().child(self.render_tab(cx)));
        h_flex()
            .size_full()
            .child(self.interaction_content(zen.into_any_element(), cx))
    }

    fn render_unsaved(&self, cx: &Context<Self>) -> Option<impl IntoElement + use<>> {
        let unsaved = self.unsaved_data(cx);
        (!unsaved.is_empty()).then(|| {
            h_flex()
                .id("unsaved-data")
                .px_3()
                .py_2()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .child(format!("Unsaved data: {}", unsaved.join("; "))),
                )
                .child(
                    Button::new("retry-save")
                        .label("Retry save")
                        .small()
                        .on_click(cx.listener(|s, _, _, cx| s.retry_saves(cx))),
                )
        })
    }
}

impl Render for Shark {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.prune_reorder(cx);
        // Toasts clear the toolbar, Features rail, and visible Files controls.
        let mut side = if self.right_rail_shown() {
            px(72.)
        } else {
            px(0.)
        };
        if self.tree_shown(cx) {
            side += self.differ.read(cx).files_width(cx);
        }
        let top = if self.zen_shown() { px(60.) } else { px(104.) };
        if cx.theme().notification.margins.right != side + px(16.)
            || cx.theme().notification.margins.top != top
        {
            let margins = &mut Theme::global_mut(cx).notification.margins;
            margins.right = side + px(16.);
            margins.top = top;
        }
        let theme = cx.theme();
        h_flex()
            .map(|d| self.reorder_events(d, cx))
            // `Vim` turns on the Vim-style list keys (see `bind_keys`).
            .key_context(if self.state.vim { "Shark Vim" } else { "Shark" })
            .size_full()
            .bg(theme.sidebar)
            .text_color(theme.foreground)
            .font_features(inter_features())
            .on_action(cx.listener(|this, _: &TogglePalette, w, cx| this.toggle_palette(w, cx)))
            .on_action(cx.listener(|this, _: &NewWorkspace, w, cx| {
                this.new_workspace(this.state.last_repos.clone(), None, w, cx)
            }))
            .on_action(cx.listener(|this, _: &ArchiveWorkspace, w, cx| this.archive(false, w, cx)))
            .on_action(
                cx.listener(|this, _: &ForceArchiveWorkspace, w, cx| this.archive(true, w, cx)),
            )
            .on_action(cx.listener(|this, _: &AddRepo, w, cx| this.add_repo(w, cx)))
            .on_action(
                cx.listener(|this, _: &CycleOpenApp, w, cx| this.cycle_open_app(false, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &OpenWorkspacePicker, w, cx| {
                    this.open_workspace_picker(w, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &ToggleHome, w, cx| this.toggle_home(w, cx)))
            .on_action(cx.listener(|this, _: &AddRepoToWorkspace, w, cx| {
                this.pick_repo_for_workspace(w, cx)
            }))
            .on_action(cx.listener(|this, _: &StopAgent, _, cx| this.stop(cx)))
            .on_action(cx.listener(|this, _: &FocusQueue, w, cx| this.focus_queue(w, cx)))
            .on_action(cx.listener(|this, _: &ShowUsage, w, cx| this.show_usage(w, cx)))
            .on_action(cx.listener(|this, _: &ShowChat, w, cx| this.show(Tab::Chat, w, cx)))
            .on_action(cx.listener(|this, _: &NewChat, w, cx| this.new_chat(w, cx)))
            .on_action(cx.listener(|this, _: &CloseChat, w, cx| this.close_chat(w, cx)))
            .on_action(cx.listener(|this, _: &CloseTab, w, cx| this.close_tab(w, cx)))
            .on_action(cx.listener(|this, _: &ReopenChat, w, cx| this.reopen_chat(None, w, cx)))
            .on_action(cx.listener(|this, _: &NextChat, w, cx| this.cycle_chat(1, w, cx)))
            .on_action(cx.listener(|this, _: &PrevChat, w, cx| this.cycle_chat(-1, w, cx)))
            .on_action(cx.listener(|this, _: &NextTab, w, cx| this.cycle_tab(1, w, cx)))
            .on_action(cx.listener(|this, _: &PrevTab, w, cx| this.cycle_tab(-1, w, cx)))
            .on_action(cx.listener(|this, a: &OpenTab, w, cx| this.select_tab(a.0, w, cx)))
            .on_action(cx.listener(|this, _: &OpenSide, w, cx| this.open_side(w, cx)))
            .on_action(cx.listener(|this, _: &ShowDiff, w, cx| {
                this.show(Tab::Feature(Feature::Diff), w, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ShowGit, w, cx| {
                    this.show(Tab::Feature(Feature::Git), w, cx)
                }),
            )
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
            .on_action(cx.listener(|this, _: &BranchMenu, w, cx| this.open_branch_menu(w, cx)))
            .on_action(
                cx.listener(|this, _: &GitFetch, w, cx| this.with_sync(w, cx, GitSync::fetch_now)),
            )
            .on_action(cx.listener(|this, _: &PickModel, w, cx| this.pick_model(w, cx)))
            .on_action(cx.listener(|this, _: &TogglePlanMode, w, cx| {
                this.with_plan(w, cx, |c, _, cx| c.set_plan_mode(!c.meta.plan, cx))
            }))
            .on_action(cx.listener(|this, _: &ToggleFastMode, w, cx| this.toggle_fast(w, cx)))
            .on_action(cx.listener(|this, _: &RaiseEffort, w, cx| this.step_effort(1, w, cx)))
            .on_action(cx.listener(|this, _: &LowerEffort, w, cx| this.step_effort(-1, w, cx)))
            .on_action(cx.listener(|this, _: &TogglePlan, w, cx| {
                this.with_plan(w, cx, |c, w, cx| c.toggle_plan(w, cx))
            }))
            .on_action(cx.listener(|this, _: &ApprovePlan, w, cx| {
                this.with_plan(w, cx, |c, w, cx| c.approve_plan(w, cx))
            }))
            .on_action(
                cx.listener(|this, _: &BuildPlanInNewChat, w, cx| {
                    this.build_plan_in_new_chat(w, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &RevealInFinder, w, cx| {
                this.with_target(w, cx, |ws, _, cx| cx.reveal_path(&ws.dir()))
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
            // Held modifiers reveal workspace and chat number shortcuts.
            .on_modifiers_changed(cx.listener(|this, ev: &ModifiersChangedEvent, w, cx| {
                this.app_switcher_modifiers(ev.modifiers.platform, w, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &FocusLeft, w, cx| this.move_focus(-1, w, cx)))
            .on_action(cx.listener(|this, _: &FocusRight, w, cx| this.move_focus(1, w, cx)))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            .on_action(cx.listener(|this, _: &ToggleZen, w, cx| this.toggle_zen(w, cx)))
            .on_action(cx.listener(|this, _: &ToggleFileTree, w, cx| this.toggle_tree(w, cx)))
            .on_action(cx.listener(|this, _: &ToggleVim, w, cx| this.toggle_vim(w, cx)))
            .on_action(cx.listener(|this, _: &CloseFile, w, cx| this.close_file(w, cx)))
            // On the root, not the editor, so File > Save works with focus in the tree.
            .on_action(cx.listener(|this, _: &SaveFile, w, cx| this.save(w, cx)))
            .on_action(
                cx.listener(|this, _: &EditWorkingCopy, w, cx| this.edit_working_copy(None, w, cx)),
            )
            // `o` in the diff and the tree.
            .on_action(cx.listener(|this, _: &OpenFile, w, cx| this.open_cursor_file(w, cx)))
            .on_action(cx.listener(|this, _: &OpenAnyFile, w, cx| this.open_any_file(w, cx)))
            .on_action(cx.listener(|this, _: &RunScripts, w, cx| this.toggle_run(w, cx)))
            .on_action(cx.listener(|this, _: &OpenSettings, w, cx| {
                crate::settings::open(&this.state, w, cx)
            }))
            .children(self.menu.as_ref().map(|(m, _)| menu::layer(m.clone())))
            .children(self.render_app_switcher(window, cx))
            .children(self.render_usage(window, cx))
            .when(self.zen_shown(), |d| d.bg(cx.theme().background))
            .child(match self.zen_shown() {
                true => self.render_zen(cx),
                false => {
                    // Content sits below the titlebar, touching a rail or inset 8px beside an expanded pane.
                    let island = div()
                        .size_full()
                        .pb(px(8.))
                        .min_h_0()
                        .flex_1()
                        .when(!self.rail, |d| d.pl(px(8.)))
                        .when(!self.right_rail_shown(), |d| d.pr(px(8.)))
                        .child(
                            h_flex()
                                .id("content-island")
                                .size_full()
                                .rounded(cx.theme().radius_lg)
                                .border_1()
                                .border_color(cx.theme().border)
                                .bg(cx.theme().background)
                                .overflow_hidden()
                                .child(self.render_main(window, cx))
                                .test_support(),
                        );
                    let main = v_flex()
                        .h_full()
                        .flex_1()
                        .min_w_0()
                        .child(self.render_titlebar(window, cx))
                        .children(self.render_unsaved(cx))
                        .child(island);
                    let main = h_flex()
                        .size_full()
                        .min_w_0()
                        .child(main)
                        .when(self.right_rail_shown(), |d| {
                            d.child(self.render_features(window, cx))
                        })
                        .into_any_element();
                    let main = self.interaction_content(main, cx);
                    let sidebar = self.render_sidebar(window, cx).into_any_element();
                    match self.rail {
                        true => h_flex().size_full().child(sidebar).child(main),
                        // Dragged like Files, between 200 and 480.
                        false => h_flex().size_full().child(
                            h_resizable("sidebar-panel")
                                .with_state(&self.sidebar_resize)
                                .with_handle_appearance(quiet_edge())
                                .child(
                                    resizable_panel()
                                        .size(px(self.state.sidebar_width.unwrap_or(244.)))
                                        .size_range(px(200.)..px(480.))
                                        .fixed()
                                        .child(sidebar),
                                )
                                .child(resizable_panel().min_w_0().child(main)),
                        ),
                    }
                }
            })
    }
}
