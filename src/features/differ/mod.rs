//! Differ owns its presentation, edits, reviews and asynchronous work. The host supplies
//! workspace snapshots and handles only requests that cross feature boundaries.
use super::FeatureFocus;
use crate::checkbox::Checkbox;
use crate::diff_view::{Diff, DiffEvent, DiffFile, DiffFileStatus, DiffMode, DiffState};
use crate::files::{self, Row};
use crate::menu::{self, Entry};
use crate::state::{State, Workspace};
use crate::ui::{
    confirm, copy, faint, kbd, list_cursor_row, notify_err, quiet_edge, side_header, side_row,
};
use crate::{
    AddComment, CancelEdit, CollapseDir, Confirm, DeleteThread, EditThread, EditWorkingCopy,
    ExpandDir, NextFile, NextHunk, NextThread, OpenFile, PageDown, PageUp, PlayMedia, PrevFile,
    PrevHunk, PrevThread, RefreshDiff, ReopenThread, RequeueOutdated, ResolveAddressed,
    ResolveThread, SaveFile, SelectFirst, SelectLast, SelectNext, SelectPrev, SendComments,
    SubmitForm, SuggestChange, ToggleDiffMode, ToggleRow, ToggleViewed, ViewChange, git,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::Dialog;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{
    ResizablePanelEvent, ResizableState, h_resizable, resizable_panel,
};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon, Sizable as _, Theme, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
mod diff;
mod inline_edit;
use crate::media;
mod review;
mod review_comment;
mod review_send;
mod review_share;
mod review_store;
#[cfg(all(test, feature = "test-support"))]
pub(crate) mod test_support;
pub(crate) use diff::Viewed;

#[derive(Clone)]
pub(crate) struct Preferences {
    pub(crate) unified: bool,
    pub(crate) uncommitted_only: bool,
    tree_width: Option<f32>,
    split_ratio: Option<f32>,
    vim: bool,
}
impl From<&State> for Preferences {
    fn from(s: &State) -> Self {
        Self {
            unified: s.unified,
            uncommitted_only: s.uncommitted_only,
            tree_width: s.tree_width,
            split_ratio: s.split_ratio,
            vim: s.vim,
        }
    }
}

#[derive(Clone)]
pub(crate) enum Preference {
    Unified(bool),
    UncommittedOnly(bool),
    TreeWidth(f32),
    SplitRatio(f32),
}

/// Effective comparison bases and PR context are captured by the host from GitSync.
#[derive(Clone)]
pub(crate) struct WorkspaceSnapshot {
    pub workspace: Workspace,
    pub prs: HashMap<String, (crate::sync::Pr, bool)>,
}

#[derive(Clone)]
pub(crate) enum DifferEvent {
    OpenApp {
        app: &'static crate::apps::OpenApp,
        path: PathBuf,
    },
    Activate(String),
    DiscardEdit {
        edit: EditSnapshot,
        action: AfterDiscard,
    },
    OpenEditor {
        workspace: String,
        path: String,
        new_tab: bool,
        existing_only: bool,
    },
    OpenChat {
        workspace: String,
        chat: Option<String>,
    },
    EditWorkingCopy {
        workspace: String,
        path: SharedString,
    },
    DiscardFile {
        workspace: String,
        path: String,
    },
    Interacted(String),
    Viewed {
        workspace: String,
        viewed: HashMap<String, u64>,
    },
    Preference(Preference),
    SendReview(review_send::ReviewRequest),
}
impl EventEmitter<DifferEvent> for Differ {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EditSnapshot {
    pub workspace: String,
    pub path: SharedString,
    pub abs: PathBuf,
    pub input: EntityId,
    pub revision: u64,
    pub dirty: bool,
    pub saving: bool,
}

pub(crate) struct Differ {
    workspace: Option<Workspace>,
    preferences: Preferences,
    prs: HashMap<String, (crate::sync::Pr, bool)>,
    editor_path: Option<String>,
    diff: Entity<DiffState>,
    /// Changed files in tree order, with each patch's fingerprint.
    diff_files: Vec<DiffFile>,
    diff_fps: Vec<u64>,
    diff_fp: u64,
    /// Bases used by the displayed patch, including the PR target discovered by polling.
    diff_bases: HashMap<String, String>,
    /// Per repo, the commit the displayed patch is against (old-side lines are numbered in it).
    diff_revs: HashMap<String, String>,
    /// Bumped per refresh, so a slow older refresh can't overwrite a newer one.
    diff_gen: u64,
    /// The file last moved to (`}`/`{`, the tree); the diff can't say which one is on screen.
    diff_file: usize,
    /// False until the first refresh of the open workspace lands.
    diff_loaded: bool,
    diff_error: Option<String>,
    /// Git status paths, independently of the diff's selected comparison base.
    uncommitted: HashSet<String>,
    /// Paths currently viewed (fingerprint still matches), for the diff headers.
    viewed_now: Rc<HashSet<SharedString>>,
    tree_rows: Vec<Row>,
    collapsed_dirs: HashSet<String>,
    tree_sel: usize,
    tree_focus: FocusHandle,
    tree_scroll: UniformListScrollHandle,
    /// The Files panel's "Filter files…" box, and its text the tree filters by.
    tree_filter: Entity<InputState>,
    tree_query: String,
    /// The Diff artifact's internal Files split; its width persists in `State`.
    tree_resize: Entity<ResizableState>,
    show_tree: bool,
    editing: Option<inline_edit::InlineEdit>,
    review: review::Review,
    media: Vec<(SharedString, Entity<media::MediaView>)>,
    media_cache: Entity<media::Lru>,
    media_width: Rc<std::cell::Cell<Pixels>>,
    menu: Option<(Entity<menu::Menu>, Subscription)>,
    _subscriptions: Vec<Subscription>,
}

impl Differ {
    pub(crate) fn new(
        preferences: Preferences,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let tree_filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter files…"));
        let diff = cx.new(|cx| {
            let mut d = DiffState::new(vec![], cx).with_mode(diff::diff_mode(preferences.unified));
            if let Some(ratio) = preferences.split_ratio {
                d.set_split_ratio(ratio, cx);
            }
            d
        });
        let tree_resize = cx.new(|_| ResizableState::default());
        cx.background_executor()
            .spawn(async { media::prune_cache() })
            .detach();
        let subscriptions = vec![
            cx.observe(&diff, |_, _, cx| cx.notify()),
            cx.subscribe(&tree_resize, |s, r, _: &ResizablePanelEvent, cx| {
                if let Some(w) = r.read(cx).sizes().get(1) {
                    s.preferences.tree_width = Some(w.as_f32());
                    cx.emit(DifferEvent::Preference(Preference::TreeWidth(w.as_f32())));
                }
            }),
            cx.subscribe_in(
                &tree_filter,
                window,
                |s, input, ev: &InputEvent, w, cx| match ev {
                    InputEvent::Change => s.filter_tree(input.read(cx).value().to_string(), cx),
                    InputEvent::PressEnter { .. } => s.filter_jump(w, cx),
                    _ => {}
                },
            ),
            cx.subscribe(
                &diff,
                |s, _, DiffEvent::SplitRatioChanged(r): &DiffEvent, cx| {
                    s.preferences.split_ratio = Some(*r);
                    cx.emit(DifferEvent::Preference(Preference::SplitRatio(*r)));
                },
            ),
        ];
        Self {
            workspace: None,
            preferences,
            prs: HashMap::new(),
            editor_path: None,
            diff,
            diff_files: vec![],
            diff_fps: vec![],
            diff_fp: 0,
            diff_bases: HashMap::new(),
            diff_revs: HashMap::new(),
            diff_gen: 0,
            diff_file: 0,
            diff_loaded: false,
            diff_error: None,
            uncommitted: HashSet::new(),
            viewed_now: Rc::default(),
            tree_rows: vec![],
            collapsed_dirs: HashSet::new(),
            tree_sel: 0,
            tree_focus: cx.focus_handle(),
            tree_scroll: UniformListScrollHandle::new(),
            tree_filter,
            tree_query: String::new(),
            tree_resize,
            show_tree: true,
            editing: None,
            review: Default::default(),
            media: vec![],
            media_cache: media::Lru::new(cx),
            media_width: Rc::default(),
            menu: None,
            _subscriptions: subscriptions,
        }
    }

    fn active_ws(&self) -> Option<&Workspace> {
        self.workspace.as_ref()
    }
    pub(crate) fn workspace_name(&self) -> Option<&str> {
        self.workspace.as_ref().map(|ws| ws.name.as_str())
    }

    pub(crate) fn set_workspace(
        &mut self,
        snapshot: Option<WorkspaceSnapshot>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = snapshot.as_ref().map(|s| s.workspace.name.as_str());
        if self.workspace_name() != name {
            // Typed words stay a pending comment of the workspace being left.
            self.keep_draft(window, cx);
            self.diff_gen += 1;
            self.editing = None;
            self.diff_fp = 0;
            self.diff_bases.clear();
            self.diff_revs.clear();
            self.diff_files.clear();
            self.diff_fps.clear();
            self.viewed_now = Rc::default();
            self.diff_file = 0;
            self.diff_loaded = false;
            self.diff_error = None;
            self.uncommitted.clear();
            self.tree_rows.clear();
            self.collapsed_dirs.clear();
            self.tree_sel = 0;
            self.lose_view();
            self.review.composer = None;
            self.review.sheet = None;
            self.close_share(window, cx);
            self.media.clear();
            self.menu = None;
            self.diff.update(cx, |d, cx| d.set_files([], cx));
        }
        self.update_workspace(snapshot);
        cx.notify();
    }

    pub(crate) fn update_workspace(&mut self, mut snapshot: Option<WorkspaceSnapshot>) {
        // Viewed marks originate here; a host snapshot can arrive before their persistence event.
        if let (Some(current), Some(next)) = (&self.workspace, &mut snapshot)
            && current.name == next.workspace.name
        {
            next.workspace.viewed = current.viewed.clone();
        }
        self.prs = snapshot.as_ref().map(|s| s.prs.clone()).unwrap_or_default();
        self.workspace = snapshot.map(|s| s.workspace);
    }

    pub(crate) fn configure(&mut self, vim: bool, editor_path: Option<String>) {
        self.preferences.vim = vim;
        self.editor_path = editor_path;
    }

    pub(crate) fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_diff(cx);
        // Deferred activation must preserve the comment input just focused by a menu action.
        if let Some(composer) = &self.review.composer
            && std::iter::once(&composer.input)
                .chain(composer.suggest.iter())
                .any(|input| input.read(cx).focus_handle(cx).contains_focused(window, cx))
        {
            return;
        }
        if self.editing.is_some() {
            self.focus_edit(window, cx);
        } else {
            self.focus_diff(window, cx);
        }
    }

    /// True while `path` is being edited in the diff with unsaved or in-flight changes.
    pub(crate) fn has_unsaved_edit(&self, path: &str) -> bool {
        (self.editing.as_ref()).is_some_and(|e| e.path == path && (e.dirty || e.saving))
    }

    pub(crate) fn edit_snapshot(&self) -> Option<EditSnapshot> {
        let e = self.editing.as_ref()?;
        Some(EditSnapshot {
            workspace: self.workspace_name()?.into(),
            path: e.path.clone(),
            abs: e.abs.clone(),
            revision: files::fingerprint(e.buf.text()),
            input: e.input.entity_id(),
            dirty: e.dirty,
            saving: e.saving,
        })
    }

    pub(crate) fn clear_dirty(&mut self, expected: &EditSnapshot) -> bool {
        if self.edit_snapshot().as_ref() != Some(expected) {
            return false;
        }
        if let Some(e) = &mut self.editing {
            e.dirty = false;
        }
        true
    }

    /// The file list or its filter has the focus.
    fn tree_has_focus(&self, window: &Window, cx: &App) -> bool {
        self.tree_focus.is_focused(window)
            || (self.tree_filter.read(cx).focus_handle(cx)).is_focused(window)
    }

    pub(crate) fn focus_targets(&self) -> Vec<FeatureFocus> {
        let mut targets = vec![FeatureFocus::Main];
        if self.tree_shown() {
            targets.push(FeatureFocus::Files);
        }
        targets
    }

    pub(crate) fn focused(&self, window: &Window, cx: &App) -> Option<FeatureFocus> {
        if self.tree_shown() && self.tree_has_focus(window, cx) {
            Some(FeatureFocus::Files)
        } else if self
            .diff
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx)
            || self.editing.as_ref().is_some_and(|e| e.focused(window, cx))
        {
            Some(FeatureFocus::Main)
        } else {
            None
        }
    }

    pub(crate) fn focus(
        &mut self,
        target: FeatureFocus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            FeatureFocus::Files if self.tree_shown() => self.tree_focus.focus(window, cx),
            FeatureFocus::Files => {}
            FeatureFocus::Main => self.focus_diff(window, cx),
        }
    }

    pub(crate) fn files_width(&self, cx: &App) -> Pixels {
        if !self.tree_shown() {
            return px(0.);
        }
        self.tree_resize
            .read(cx)
            .sizes()
            .get(1)
            .copied()
            .unwrap_or(px(self.preferences.tree_width.unwrap_or(244.)))
    }

    pub(crate) fn toggle_tree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_tree = !self.show_tree;
        if !self.show_tree && self.tree_has_focus(window, cx) {
            self.focus_diff(window, cx);
        }
        cx.notify();
    }

    fn show_menu(
        &mut self,
        pos: Point<Pixels>,
        entries: Vec<Entry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let m = menu::open(entries, pos, window, cx);
        menu::hold(self, m, |s| &mut s.menu, cx);
    }
}

fn on_differ(
    workspace: Option<&str>,
    cx: &Context<Differ>,
    f: impl Fn(&mut Differ, &mut Window, &mut Context<Differ>) + 'static,
) -> impl Fn(&mut Window, &mut App) + 'static {
    let entity = cx.entity().downgrade();
    let workspace = workspace.map(str::to_owned);
    move |window, cx| {
        entity
            .update(cx, |s, cx| {
                if s.workspace_name() == workspace.as_deref() {
                    f(s, window, cx);
                }
            })
            .ok();
    }
}

impl Render for Differ {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(msg) = self.review.store.warning.take() {
            window.push_notification(Notification::warning(msg).autohide(false), cx);
        }
        div()
            .flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .on_action(cx.listener(|d, _: &SaveFile, w, cx| {
                if d.editing.is_some() {
                    d.save_edit(false, w, cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(
                cx.listener(|d, _: &EditWorkingCopy, w, cx| d.edit_working_copy(None, w, cx)),
            )
            .on_action(cx.listener(|d, _: &OpenFile, w, cx| {
                if let Some(ix) = d.cursor_file(w, cx) {
                    d.open_file(ix, w, cx);
                }
            }))
            .child(self.with_tree(self.render_diff(window, cx), window, cx))
            .children(self.menu.as_ref().map(|(m, _)| menu::layer(m.clone())))
    }
}

#[derive(Clone)]
pub(crate) enum AfterDiscard {
    LeaveEdit,
    ToggleUncommitted,
    OpenPath(String),
}

pub(crate) struct Status {
    pub files: usize,
    pub viewed: usize,
    pub loaded: bool,
    pub failed: bool,
}

impl Differ {
    pub(crate) fn status(&self) -> Status {
        Status {
            files: self.diff_files.len(),
            viewed: self.viewed_now.len(),
            loaded: self.diff_loaded,
            failed: self.diff_error.is_some(),
        }
    }

    pub(crate) fn edit_target(
        &self,
        path: Option<SharedString>,
        window: &Window,
        cx: &App,
    ) -> Option<SharedString> {
        path.or_else(|| {
            self.diff
                .read(cx)
                .selected_lines()
                .map(|r| r.path().clone())
        })
        .or_else(|| self.cursor_path(window, cx))
    }

    /// Whether the diff has `path`; true while it is still loading, which says nothing yet.
    pub(crate) fn in_diff(&self, path: &str) -> bool {
        !self.diff_loaded || self.diff_files.iter().any(|f| f.path() == path)
    }

    pub(crate) fn cursor_path(&self, window: &Window, cx: &App) -> Option<SharedString> {
        self.cursor_file(window, cx)
            .map(|ix| self.diff_files[ix].path().clone())
    }

    pub(crate) fn viewed_path(&self, path: &str) -> Option<Viewed> {
        self.diff_files
            .iter()
            .position(|f| f.path() == path)
            .map(|ix| self.viewed(ix))
    }

    fn request_activation(&self, cx: &mut Context<Self>) {
        if let Some(workspace) = self.workspace_name() {
            cx.emit(DifferEvent::Activate(workspace.into()));
        }
    }

    pub(crate) fn open_cursor_file(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.cursor_file(window, cx) {
            self.open_file(ix, window, cx);
        }
    }

    fn request_editor(
        &self,
        path: String,
        new_tab: bool,
        existing_only: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(workspace) = self.workspace_name() {
            cx.emit(DifferEvent::OpenEditor {
                workspace: workspace.into(),
                path,
                new_tab,
                existing_only,
            });
        }
    }

    pub(crate) fn open_file(&self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.open_file_in_tab(ix, false, window, cx);
    }
    fn open_file_tab(&self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.open_file_in_tab(ix, true, window, cx);
    }
    fn open_file_in_tab(
        &self,
        ix: usize,
        new_tab: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(f) = self.diff_files.get(ix) else {
            return;
        };
        self.request_editor(
            f.path().to_string(),
            new_tab,
            f.status() == DiffFileStatus::Deleted || f.is_binary(),
            cx,
        );
    }

    pub(crate) fn open_path(
        &mut self,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(ix) = self.diff_files.iter().position(|f| f.path() == path) else {
            notify_err(
                window,
                cx,
                "This file is not in the current diff. Open the comment on GitHub to see its original context.",
            );
            return false;
        };
        if self.editing.as_ref().is_some_and(|e| e.path != path) {
            self.request_discard(AfterDiscard::OpenPath(path.into()), window, cx);
            return false;
        }
        self.follow_tree(ix);
        self.diff
            .update(cx, |d, cx| d.set_file_collapsed(path, false, cx));
        self.goto_file(ix, cx);
        cx.notify();
        true
    }

    fn request_discard(
        &mut self,
        action: AfterDiscard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(edit) = self.edit_snapshot() {
            cx.emit(DifferEvent::DiscardEdit { edit, action });
        } else {
            self.after_discard(action, window, cx);
        }
    }

    pub(crate) fn after_discard(
        &mut self,
        action: AfterDiscard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.end_edit(cx);
        match action {
            AfterDiscard::LeaveEdit => {
                self.refresh_diff(cx);
                self.focus_diff(window, cx);
            }
            AfterDiscard::ToggleUncommitted => {
                self.preferences.uncommitted_only = !self.preferences.uncommitted_only;
                cx.emit(DifferEvent::Preference(Preference::UncommittedOnly(
                    self.preferences.uncommitted_only,
                )));
                self.refresh_diff(cx);
                self.focus_diff(window, cx);
            }
            AfterDiscard::OpenPath(path) => {
                if self.open_path(&path, window, cx) {
                    self.request_activation(cx);
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn file_discarded(
        &mut self,
        workspace: &str,
        path: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.workspace_name() != Some(workspace) {
            return false;
        }
        let ended_edit = self.editing.as_ref().is_some_and(|e| e.path == path);
        if ended_edit {
            self.end_edit(cx);
        }
        self.uncommitted.remove(path);
        ended_edit
    }

    pub(crate) fn storage_errors(&self) -> Vec<(String, String)> {
        self.review
            .store
            .failed_saves
            .iter()
            .map(|(ws, e)| (ws.clone(), e.clone()))
            .collect()
    }
    pub(crate) fn retry_saves(&mut self) {
        self.review.store.retry();
    }
    pub(crate) fn forget_workspace(&mut self, name: &str) {
        self.review.store.forget(name);
    }
    pub(crate) fn unqueue_review(
        &mut self,
        name: &str,
        chat: &str,
        unread: &[u64],
        cx: &mut Context<Self>,
    ) {
        self.review.store.edit(name, |l| {
            review_store::unqueue(l, Some(chat), unread);
        });
        cx.notify();
    }
}
