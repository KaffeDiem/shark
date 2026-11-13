//! Task-qualified integration with Differ. Destructive coordination stays in the host.
use super::*;
use crate::state::ChatMeta;
use differ::{DifferEvent, TaskSnapshot};

impl Shark {
    /// `task` with GitSync's effective comparison bases, as Diff and Git use them.
    pub(super) fn effective_task(&self, mut task: Task, cx: &App) -> Task {
        if let Some(sync) = self.syncs.get(&task.name) {
            let sync = sync.read(cx);
            for repo in &mut task.repos {
                if let Some(base) = sync.diff_base(&repo.name) {
                    repo.base = base;
                }
            }
        }
        task
    }

    fn differ_task(&self, cx: &App) -> Option<TaskSnapshot> {
        let task = self.effective_task(self.active_task()?.clone(), cx);
        let mut prs = HashMap::new();
        if let Some(sync) = self.active_sync() {
            let sync = sync.read(cx);
            for repo in &task.repos {
                if let Some(pr) = sync.pr_for(&repo.name) {
                    prs.insert(repo.name.clone(), pr);
                }
            }
        }
        Some(TaskSnapshot { task, prs })
    }

    /// A moved comparison base reaches the retained Git entity, which resets only its history.
    pub(super) fn configure_git(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let (Some(ws), Some(git)) = (self.active_task(), self.active_git_feature()) {
            let ws = self.effective_task(ws.clone(), cx);
            git.update(cx, |g, cx| g.set_task(ws, window, cx));
        }
    }

    pub(super) fn configure_differ(&self, cx: &mut Context<Self>) {
        let snapshot = self.differ_task(cx);
        let editor_path = self.editor.as_ref().map(|e| e.read(cx).path.clone());
        self.differ.update(cx, |d, _| {
            d.update_task(snapshot);
            d.configure(self.state.vim, editor_path);
        });
    }

    pub(super) fn reset_differ_task(&self, window: &mut Window, cx: &mut Context<Self>) {
        let snapshot = self.differ_task(cx);
        self.differ
            .update(cx, |d, cx| d.set_task(snapshot, window, cx));
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
        if let (Some(task), Some(path)) = (
            self.active.clone(),
            self.differ.read(cx).edit_target(path, window, cx),
        ) {
            self.differ_event(&DifferEvent::EditWorkingCopy { task, path }, window, cx);
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

    /// Artifacts' requests; paths are task-relative.
    pub(super) fn artifacts_event(
        &mut self,
        event: &artifacts::ArtifactsEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use artifacts::ArtifactsEvent::*;
        match event {
            OpenEditor(path) => self.open_path(path.to_string_lossy().into_owned(), window, cx),
            AttachToChat(path) => {
                self.show(Tab::Chat, window, cx);
                if let Some(chat) = self.active_chat().cloned() {
                    let label = path
                        .file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().into());
                    chat.update(cx, |c, cx| {
                        c.attach_mention(&path.to_string_lossy(), label, window, cx);
                        c.focus_composer(window, cx);
                    });
                }
            }
            Width(width) => {
                self.state.artifacts_width = Some(*width);
                let _ = self.state.save();
            }
            Pins(pins) => {
                if let Some(ws) = self.active_task_mut() {
                    ws.artifacts_pinned = pins.clone();
                    let _ = self.state.save();
                }
            }
            ShowPlanInChat(id) => {
                let ix =
                    (self.active_task()).and_then(|ws| ws.chats.iter().position(|c| &c.id == id));
                if let Some(ix) = ix {
                    self.select_chat(ix, window, cx);
                    self.show(Tab::Chat, window, cx);
                    if let Some(chat) = self.active_chat().cloned() {
                        chat.update(cx, |c, cx| {
                            if !c.plan_open {
                                c.toggle_plan(window, cx);
                            }
                        });
                    }
                }
            }
        }
    }

    pub(super) fn differ_event(
        &mut self,
        event: &DifferEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            DifferEvent::OpenApp { app, path } => self.launch_app(app, path),
            DifferEvent::Activate(task) if self.active.as_ref() == Some(task) => {
                self.show(Tab::Feature(Feature::Diff), window, cx)
            }
            DifferEvent::OpenEditor {
                task,
                path,
                new_tab,
                existing_only,
            } if self.active.as_ref() == Some(task) => {
                if *existing_only && self.file_tab_ix(path, cx).is_none() {
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
            DifferEvent::OpenChat { task, chat } if self.active.as_ref() == Some(task) => {
                if let Some(ix) = self
                    .active_task()
                    .and_then(|ws| ws.chats.iter().position(|m| Some(&m.id) == chat.as_ref()))
                {
                    self.select_chat(ix, window, cx);
                }
                self.show(Tab::Chat, window, cx);
            }
            DifferEvent::EditWorkingCopy { task, path } if self.active.as_ref() == Some(task) => {
                let (task, path) = (task.clone(), path.clone());
                if self
                    .differ
                    .read(cx)
                    .edit_snapshot()
                    .is_some_and(|e| e.task == task && e.path == path)
                {
                    return self.show(Tab::Feature(Feature::Diff), window, cx);
                }
                // begin_edit would find nothing after the switch and do nothing.
                if !self.differ.read(cx).in_diff(&path) {
                    return notify_err(window, cx, format!("{path} is not in the current diff"));
                }

                if self.editor_busy(&path, cx) {
                    return notify_err(
                        window,
                        cx,
                        format!("{path} has unsaved edits in the editor"),
                    );
                }
                self.confirm_discard(false, true, window, cx, move |s, w, cx| {
                    if s.active.as_ref() != Some(&task) {
                        return;
                    }
                    if s.editor_busy(&path, cx) {
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
            DifferEvent::DiscardFile { task, path } => {
                self.discard_uncommitted(task, path, window, cx)
            }
            DifferEvent::Interacted(task) => self.record_interaction(task, cx),
            DifferEvent::Viewed { task, viewed } => {
                if let Some(ws) = self.task_mut(task) {
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
                if self.active.as_ref() != Some(&request.task)
                    || !self.differ.read(cx).review_request_valid(request)
                {
                    return notify_err(window, cx, "The diff changed; open the sheet again");
                }
                let Some(ws) = self.active_task().cloned() else {
                    return;
                };
                // A new chat is a tab like cmd-t's, but the diff stays on screen.
                let meta = match &request.target {
                    differ::Target::Chat(id) => ws.chats.iter().find(|m| &m.id == id).cloned(),
                    differ::Target::New(pick) => {
                        let id = self.active_task_mut().unwrap().next_chat_id();
                        let meta = ChatMeta {
                            model: pick.model.clone(),
                            effort: pick.effort.clone(),
                            ..self.state.new_chat(&ws, id, None)
                        };
                        self.active_task_mut().unwrap().push_chat(meta.clone());
                        let _ = self.state.save();
                        self.configure_differ(cx); // the next sheet lists it
                        Some(meta)
                    }
                };
                let Some(meta) = meta else {
                    return notify_err(window, cx, "That chat was closed");
                };
                let ws = self.active_task().cloned().unwrap();
                let chat = self.ensure_chat(&ws, &meta, window, cx);
                if !chat.update(cx, |c, cx| c.send_review(request.card.clone(), cx)) {
                    // A new chat shows its refusal; the next send's "This chat" is it, so
                    // no second one is made.
                    if matches!(request.target, differ::Target::New(_))
                        && let Some(ix) = ws.chats.iter().position(|m| m.id == meta.id)
                    {
                        self.select_chat(ix, window, cx);
                    }
                    return notify_err(window, cx, "Review not sent; the chat says why");
                }
                let queued = chat.read(cx).queued() > 0;
                // GPUI delivers Chat events after this update. Queue first, so even an immediate
                // ReviewSent acknowledgement observes the accepted comments.
                self.differ.update(cx, |d, cx| {
                    d.review_accepted(request, &meta.id, queued, window, cx)
                });
            }
            _ => {}
        }
        cx.notify();
    }
}
