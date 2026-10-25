//! Root view: sidebar of repos and workspaces, Chat/Diff tabs, palette and dialogs.
use crate::chat::{Chat, ChatEvent, MODELS};
use crate::state::{Repo, State, workspaces_dir};
use crate::sync::{GitSync, SyncEvent};
use crate::*;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::diff::{Diff, DiffFile, DiffMode, DiffState};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
    ActiveTheme, Icon, IndexPath, Sizable as _, Theme, WindowExt, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::HashMap;
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
pub(crate) fn faint(theme: &Theme) -> Hsla {
    match theme.is_dark() {
        true => rgb(0x808083).into(),
        false => rgb(0x9c9d9f).into(),
    }
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

#[derive(PartialEq, Clone, Copy, Debug)]
enum Tab {
    Chat,
    Diff,
}

pub struct Shark {
    pub(crate) state: State,
    sel: usize,
    pub(crate) active: Option<String>,
    pub(crate) chats: HashMap<String, Entity<Chat>>,
    tab: Tab,
    diff: Entity<DiffState>,
    diff_files: Vec<String>,
    diff_file: usize,
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
    ("Show chat", IconName::MessageSquare, |s, w, cx| {
        s.show(Tab::Chat, w, cx)
    }),
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
    ("Stop agent", IconName::Square, |s, _, cx| s.stop(cx)),
    ("Change model…", IconName::Sparkles, |s, w, cx| {
        s.pick_model(w, cx)
    }),
];

impl Shark {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = State::load();
        let sidebar_focus = cx.focus_handle();
        sidebar_focus.focus(window, cx);
        Shark {
            state,
            sel: 0,
            active: None,
            chats: HashMap::new(),
            tab: Tab::Chat,
            diff: cx.new(|cx| DiffState::new(vec![], cx)),
            diff_files: vec![],
            diff_file: 0,
            syncs: HashMap::new(),
            sidebar_focus,
            _subs: vec![],
        }
    }

    fn notify_err(window: &mut Window, cx: &mut App, e: impl Into<SharedString>) {
        window.push_notification(Notification::error(e), cx);
    }

    fn active_chat(&self) -> Option<&Entity<Chat>> {
        self.chats.get(self.active.as_ref()?)
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
        let Some(ws) = self.state.workspaces.get(ix).cloned() else {
            return;
        };
        self.sel = ix;
        if !self.chats.contains_key(&ws.name) {
            let chat = cx.new(|cx| Chat::new(ws.clone(), window, cx));
            let name = ws.name.clone();
            self._subs.push(
                cx.subscribe(&chat, move |this, _, ev: &ChatEvent, cx| match ev {
                    ChatEvent::SessionId(id) => {
                        if let Some(w) = this.state.workspaces.iter_mut().find(|w| w.name == name) {
                            w.session_id = Some(id.clone());
                            this.state.save();
                        }
                    }
                    ChatEvent::Model(model) => {
                        if let Some(w) = this.state.workspaces.iter_mut().find(|w| w.name == name) {
                            w.model = model.clone();
                            this.state.save();
                        }
                    }
                    ChatEvent::TurnDone => {
                        if this.active.as_ref() == Some(&name) {
                            this.refresh_diff(cx);
                        }
                        cx.notify();
                    }
                }),
            );
            let sync = cx.new(|cx| GitSync::new(ws.clone(), chat.clone(), window, cx));
            let name = ws.name.clone();
            self._subs.push(cx.subscribe_in(
                &sync,
                window,
                move |this, _, ev: &SyncEvent, window, cx| match ev {
                    SyncEvent::Archive => this.remove_workspace(&name, false, window, cx),
                },
            ));
            self.chats.insert(ws.name.clone(), chat);
            self.syncs.insert(ws.name.clone(), sync);
        }
        for (name, sync) in &self.syncs {
            let on = *name == ws.name;
            sync.update(cx, |g, cx| g.set_active(on, cx));
        }
        self.active = Some(ws.name);
        self.refresh_diff(cx);
        self.show(Tab::Chat, window, cx);
        if let Some(chat) = self.active_chat().cloned() {
            chat.update(cx, |c, cx| c.focus_composer(window, cx));
        }
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
        cx.spawn(async move |this, cx| {
            let name = ws.name.clone();
            let patch = cx
                .background_executor()
                .spawn(async move { git::combined_diff(&ws, &ws.dir()) })
                .await;
            let files = DiffFile::parse(&patch).unwrap_or_default();
            this.update(cx, |s, cx| {
                if s.active.as_ref() != Some(&name) {
                    return; // a slower refresh for a workspace that is no longer shown
                }
                s.diff_files = files.iter().map(|f| f.path().to_string()).collect();
                s.diff_file = 0;
                s.diff.update(cx, |d, cx| d.set_files(files, cx));
                cx.notify();
            })
            .ok();
        })
        .detach();
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
        let form = cx.new(|cx| NewWorkspaceForm::new(self.state.repos.clone(), shark, window, cx));
        let input = form.read(cx).branch.clone();
        window.open_dialog(cx, move |d, _, _| {
            let f = form.clone();
            // Dialog binds enter to Confirm, which runs on_ok and closes when it returns true.
            d.font_features(inter_features())
                .title("New workspace")
                .w(px(480.))
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

    /// Archives the workspace under the sidebar cursor when the sidebar is focused, else the open one.
    fn archive(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        let target = if self.sidebar_focus.is_focused(window) {
            self.state.workspaces.get(self.sel).map(|w| w.name.clone())
        } else {
            self.active.clone()
        };
        let Some(name) = target else { return };
        if self.chats.get(&name).is_some_and(|c| c.read(cx).busy) {
            return Self::notify_err(window, cx, "Agent is running; stop it first (ctrl-c)");
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
                self.chats.remove(name);
                self.syncs.remove(name);
                self.state.workspaces.retain(|w| w.name != name);
                self.state.save();
                // A later workspace with the same name must not replay or resume this conversation.
                let _ = std::fs::remove_file(crate::agent::transcript_path(name));
                if self.active.as_deref() == Some(name) {
                    self.active = None;
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
        let header = |t: &'static str| {
            div()
                .px_4()
                .pt_4()
                .pb_1()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.muted_foreground)
                .child(t)
        };
        let hint = |t: &'static str| div().px_4().text_xs().text_color(faint(theme)).child(t);
        let row = || {
            h_flex()
                .h(px(28.))
                .flex_none()
                .mx_2()
                .px_2()
                .gap_2()
                .rounded(theme.radius)
                .text_sm()
        };
        let repos = self.state.repos.iter().map(|r| {
            row()
                .text_color(theme.muted_foreground)
                .child(Icon::new(IconName::FolderGit2).with_size(px(14.)))
                .child(div().truncate().child(r.name.clone()))
        });
        let workspaces = self.state.workspaces.iter().enumerate().map(|(ix, w)| {
            let busy = self.chats.get(&w.name).is_some_and(|c| c.read(cx).busy);
            let selected = ix == self.sel;
            let active = self.active.as_ref() == Some(&w.name);
            row()
                .id(ix)
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
                .child(match busy {
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
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .when(active, |d| d.font_weight(FontWeight::MEDIUM))
                        .child(w.name.clone()),
                )
                .child(
                    div().text_xs().text_color(theme.muted_foreground).child(
                        w.repos
                            .iter()
                            .map(|r| r.name.as_str())
                            .collect::<Vec<_>>()
                            .join(" · "),
                    ),
                )
        });
        v_flex()
            .id("sidebar")
            .key_context("Sidebar")
            .track_focus(&self.sidebar_focus)
            .w(px(244.))
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

    fn render_main(&self, cx: &mut Context<Self>) -> AnyElement {
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
        let busy = chat.read(cx).busy;
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
                    .child(tab("Chat".into(), IconName::MessageSquare, Tab::Chat))
                    .child(tab(
                        format!("Diff · {}", self.diff_files.len()),
                        IconName::FileDiff,
                        Tab::Diff,
                    ))
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
                            .child(if busy { "Running" } else { "Idle" }),
                    ),
            )
            .child(match self.tab {
                Tab::Chat => div().flex_1().min_h_0().child(chat).into_any_element(),
                Tab::Diff => div()
                    .key_context("DiffPane")
                    .flex_1()
                    .min_h_0()
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
                    .child(Diff::new(&self.diff))
                    .into_any_element(),
            })
            .into_any_element()
    }

    fn step_file(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.diff_files.is_empty() {
            return;
        }
        let n = self.diff_files.len() as isize;
        self.diff_file = (self.diff_file as isize + delta).rem_euclid(n) as usize;
        let path = self.diff_files[self.diff_file].clone();
        self.diff.update(cx, |d, cx| d.scroll_to_file(&path, cx));
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
            .on_action(cx.listener(|this, _: &ShowChat, w, cx| this.show(Tab::Chat, w, cx)))
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
            .on_action(cx.listener(|this, a: &SetModel, w, cx| this.set_model(a.0, w, cx)))
            .on_action(cx.listener(|this, _: &FocusSidebar, w, cx| {
                this.sidebar_focus.focus(w, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &FocusMain, w, cx| {
                let tab = this.tab;
                this.show(tab, w, cx)
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
                        .child(self.render_main(cx)),
                ),
            )
    }
}

/// Contents of the New workspace dialog: a branch name and the repos to include.
struct NewWorkspaceForm {
    branch: Entity<InputState>,
    repos: Vec<Repo>,
    picked: Vec<bool>,
    shark: WeakEntity<Shark>,
    _sub: Subscription,
}

impl NewWorkspaceForm {
    fn new(
        repos: Vec<Repo>,
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
        let picked = vec![true; repos.len()];
        NewWorkspaceForm {
            branch,
            repos,
            picked,
            shark,
            _sub,
        }
    }

    /// Returns whether the form was valid (and the dialog may close).
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let branch = self.branch.read(cx).value().trim().to_string();
        let repos: Vec<Repo> = self
            .repos
            .iter()
            .zip(&self.picked)
            .filter(|(_, p)| **p)
            .map(|(r, _)| r.clone())
            .collect();
        if branch.is_empty() || repos.is_empty() {
            return false;
        }
        self.shark
            .update(cx, |s, cx| s.create_workspace(branch, repos, window, cx))
            .ok();
        true
    }
}

impl Render for NewWorkspaceForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .child(Input::new(&self.branch))
            .children(self.repos.iter().enumerate().map(|(i, r)| {
                Checkbox::new(("repo", i))
                    .label(r.name.clone())
                    .checked(self.picked[i])
                    .on_click(cx.listener(move |this, v: &bool, _, cx| {
                        this.picked[i] = *v;
                        cx.notify();
                    }))
            }))
            .child(
                h_flex().justify_end().child(
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
    use std::{fs, time::Duration};

    /// Drives the real keymap headlessly: create a workspace, vim focus moves, tabs, archive.
    #[gpui_kit::test]
    async fn keyboard_flow(cx: &mut TestAppContext) {
        let home = std::env::temp_dir().join(format!("shark-ui-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let repo = home.join("src/api");
        fs::create_dir_all(&repo).unwrap();
        git::git(&repo, &["init", "-q", "-b", "main"]).unwrap();
        git::git(
            &repo,
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
        crate::state::TEST_HOME.set(home.clone()).unwrap();
        State {
            repos: vec![Repo {
                name: "api".into(),
                path: repo,
            }],
            workspaces: vec![],
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

        // Opening a workspace focuses the composer: vim keys must type, not navigate.
        cx.update_window(window, |_, w, cx| w.input("jk g", cx))
            .unwrap();
        let chat = cx.update(|cx| shark.read(cx).chats["ui-x"].clone());
        cx.update(|cx| assert_eq!(chat.read(cx).composer.read(cx).value().as_ref(), "jk g"));

        // Model shortcuts work while typing and keep the draft. Let the New workspace
        // dialog's deferred focus restore run first (it targets the sidebar), then go back.
        cx.executor().advance_clock(Duration::from_secs(1));
        press(cx, &["ctrl-w", "l", "i"]);
        let model = |cx: &mut TestAppContext| {
            cx.update(|cx| shark.read(cx).state.workspaces[0].model.clone())
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

        press(cx, &["ctrl-w", "h", "space", "w", "shift-d"]);
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
        fs::remove_dir_all(&home).unwrap();
    }
}
