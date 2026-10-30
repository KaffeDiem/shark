//! Workspace-qualified integration with Differ. Destructive coordination stays in the host.
use super::*;
use differ::{DifferEvent, WorkspaceSnapshot};

impl Shark {
    /// `workspace` with GitSync's effective comparison bases, as Diff and Git use them.
    pub(super) fn effective_workspace(&self, mut workspace: Workspace, cx: &App) -> Workspace {
        if let Some(sync) = self.syncs.get(&workspace.name) {
            let sync = sync.read(cx);
            for repo in &mut workspace.repos {
                if let Some(base) = sync.diff_base(&repo.name) {
                    repo.base = base;
                }
            }
        }
        workspace
    }

    fn differ_workspace(&self, cx: &App) -> Option<WorkspaceSnapshot> {
        let workspace = self.effective_workspace(self.active_ws()?.clone(), cx);
        let mut prs = HashMap::new();
        if let Some(sync) = self.active_sync() {
            let sync = sync.read(cx);
            for repo in &workspace.repos {
                if let Some(pr) = sync.pr_for(&repo.name) {
                    prs.insert(repo.name.clone(), pr);
                }
            }
        }
        Some(WorkspaceSnapshot { workspace, prs })
    }

    /// A moved comparison base reaches the retained Git entity, which resets only its history.
    pub(super) fn configure_git(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let (Some(ws), Some(git)) = (self.active_ws(), self.active_git_feature()) {
            let ws = self.effective_workspace(ws.clone(), cx);
            git.update(cx, |g, cx| g.set_workspace(ws, window, cx));
        }
    }

    pub(super) fn configure_differ(&self, cx: &mut Context<Self>) {
        let snapshot = self.differ_workspace(cx);
        let editor_path = self.editor.as_ref().map(|e| e.read(cx).path.clone());
        self.differ.update(cx, |d, _| {
            d.update_workspace(snapshot);
            d.configure(self.state.vim, editor_path);
        });
    }

    pub(super) fn reset_differ_workspace(&self, window: &mut Window, cx: &mut Context<Self>) {
        let snapshot = self.differ_workspace(cx);
        self.differ
            .update(cx, |d, cx| d.set_workspace(snapshot, window, cx));
    }

    pub(super) fn refresh_diff(&mut self, cx: &mut Context<Self>) {
        self.configure_differ(cx);
        self.differ.update(cx, |d, cx| d.refresh_diff(cx));
    }

    pub(super) fn with_differ(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Differ, &mut Window, &mut Context<Differ>),
    ) {
        self.configure_differ(cx);
        self.differ.update(cx, |d, cx| f(d, window, cx));
    }

    pub(super) fn tree_shown(&self, cx: &App) -> bool {
        self.tab == Tab::Feature(Feature::Diff)
            && !self.home_shown()
            && self.differ.read(cx).tree_shown()
    }

    pub(super) fn toggle_tree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == Tab::Feature(Feature::Diff) && !self.home_shown() {
            self.with_differ(window, cx, Differ::toggle_tree);
        }
    }

    pub(super) fn edit_working_copy(
        &mut self,
        path: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = path.or_else(|| {
            (self.tab == Tab::File)
                .then(|| self.editor.as_ref().map(|e| e.read(cx).path.clone().into()))
                .flatten()
        });
        self.configure_differ(cx);
        if let (Some(workspace), Some(path)) = (
            self.active.clone(),
            self.differ.read(cx).edit_target(path, window, cx),
        ) {
            self.differ_event(
                &DifferEvent::EditWorkingCopy { workspace, path },
                window,
                cx,
            );
        }
    }

    pub(super) fn open_cursor_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == Tab::File {
            if let Some(editor) = &self.editor {
                self.open_path(editor.read(cx).path.clone(), window, cx);
            }
        } else {
            self.differ
                .update(cx, |d, cx| d.open_cursor_file(window, cx));
        }
    }

    pub(super) fn toggle_cursor_viewed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = if self.tab == Tab::File {
            self.editor.as_ref().map(|e| e.read(cx).path.clone())
        } else {
            self.differ
                .read(cx)
                .cursor_path(window, cx)
                .map(|p| p.to_string())
        };
        if let Some(path) = path {
            self.differ
                .update(cx, |d, cx| d.toggle_viewed_path(&path, cx));
        }
    }

    pub(super) fn file_menu(&self, path: &str, cx: &mut Context<Self>) -> Vec<Entry> {
        self.configure_differ(cx);
        self.differ.update(cx, |d, cx| d.file_menu(path, cx))
    }

    pub(super) fn differ_event(
        &mut self,
        event: &DifferEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            DifferEvent::OpenApp { app, path } => self.launch_app(app, path),
            DifferEvent::Activate(workspace) if self.active.as_ref() == Some(workspace) => {
                self.show(Tab::Feature(Feature::Diff), window, cx)
            }
            DifferEvent::OpenEditor {
                workspace,
                path,
                new_tab,
                existing_only,
            } if self.active.as_ref() == Some(workspace) => {
                if *existing_only
                    && !self
                        .file_tabs
                        .iter()
                        .any(|t| t.editor.read(cx).path == *path)
                {
                    return notify_err(
                        window,
                        cx,
                        format!("{path} is deleted or binary; it shows in the diff only"),
                    );
                }
                if *new_tab {
                    self.open_path_tab(path.clone(), window, cx);
                } else {
                    self.open_path(path.clone(), window, cx);
                }
            }
            DifferEvent::OpenChat { workspace, chat }
                if self.active.as_ref() == Some(workspace) =>
            {
                if let Some(ix) = self
                    .active_ws()
                    .and_then(|ws| ws.chats.iter().position(|m| Some(&m.id) == chat.as_ref()))
                {
                    self.select_chat(ix, window, cx);
                }
                self.show(Tab::Chat, window, cx);
            }
            DifferEvent::EditWorkingCopy { workspace, path }
                if self.active.as_ref() == Some(workspace) =>
            {
                let (workspace, path) = (workspace.clone(), path.clone());
                if self
                    .differ
                    .read(cx)
                    .edit_snapshot()
                    .is_some_and(|e| e.workspace == workspace && e.path == path)
                {
                    return self.show(Tab::Feature(Feature::Diff), window, cx);
                }

                let conflict = |s: &Shark, cx: &App| {
                    s.file_tabs.iter().any(|t| {
                        let e = t.editor.read(cx);
                        e.path == path && (e.dirty || e.saving)
                    })
                };
                if conflict(self, cx) {
                    return notify_err(
                        window,
                        cx,
                        format!("{path} has unsaved edits in the editor"),
                    );
                }
                self.confirm_discard(false, true, window, cx, move |s, w, cx| {
                    if s.active.as_ref() != Some(&workspace) {
                        return;
                    }
                    if s.file_tabs.iter().any(|t| {
                        let e = t.editor.read(cx);
                        e.path == path && (e.dirty || e.saving)
                    }) {
                        return notify_err(
                            w,
                            cx,
                            format!("{path} has unsaved edits in the editor"),
                        );
                    }
                    s.show(Tab::Feature(Feature::Diff), w, cx);
                    s.differ.update(cx, |d, cx| d.begin_edit(path, w, cx));
                });
            }
            DifferEvent::DiscardEdit { edit, action } => {
                if self.differ.read(cx).edit_snapshot().as_ref() != Some(edit) {
                    return;
                }
                let action = action.clone();
                self.confirm_discard(false, true, window, cx, move |s, w, cx| {
                    s.differ.update(cx, |d, cx| d.after_discard(action, w, cx));
                });
            }
            DifferEvent::DiscardFile { workspace, path } => {
                self.discard_uncommitted(workspace, path, window, cx)
            }
            DifferEvent::Interacted(workspace) => self.record_interaction(workspace, cx),
            DifferEvent::Viewed { workspace, viewed } => {
                if let Some(ws) = self.ws_mut(workspace) {
                    ws.viewed = viewed.clone();
                    let _ = self.state.save();
                }
            }
            DifferEvent::Preference(preference) => {
                match preference {
                    differ::Preference::Unified(on) => self.state.unified = *on,
                    differ::Preference::UncommittedOnly(on) => self.state.uncommitted_only = *on,
                    differ::Preference::TreeWidth(width) => self.state.tree_width = Some(*width),
                    differ::Preference::SplitRatio(ratio) => self.state.split_ratio = Some(*ratio),
                }
                let _ = self.state.save();
            }
            DifferEvent::SendReview(request) => {
                if self.active.as_ref() != Some(&request.workspace)
                    || !self.differ.read(cx).review_request_valid(request)
                {
                    return;
                }
                let Some(chat) = self
                    .active_chat()
                    .filter(|chat| chat.read(cx).meta.id == request.chat)
                    .cloned()
                else {
                    return;
                };
                if !chat.update(cx, |c, cx| c.send_review(request.card.clone(), cx)) {
                    return notify_err(window, cx, "Review not sent; the chat says why");
                }
                // GPUI delivers Chat events after this update. Queue first, so even an immediate
                // ReviewSent acknowledgement observes the accepted comments.
                self.differ
                    .update(cx, |d, cx| d.review_accepted(request, window, cx));
            }
            _ => {}
        }
        cx.notify();
    }
}
