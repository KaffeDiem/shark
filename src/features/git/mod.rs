//! The Git artifact: GitHub conversations, with drafts pinned to their exact PR.
use crate::pr::git_feature as api;
use crate::state::Workspace;
use crate::ui::confirm;
use crate::{
    CancelEdit, PageDown, PageUp, SaveFile, SelectFirst, SelectLast, SelectNext, SelectPrev,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, Textarea, TextareaState};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::TextViewState;
use gpui_kit::component::{ActiveTheme, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::component::{Disableable as _, Selectable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use std::{collections::HashMap, path::PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct DescriptionDraft {
    original_title: String,
    original_body: String,
    title: String,
    body: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Draft {
    description: Option<DescriptionDraft>,
    note: String,
    replies: HashMap<String, String>,
}

#[derive(Clone, Debug, PartialEq)]
enum Editing {
    Description,
    Comment,
    Reply(String),
}

struct Composer {
    target: String,
    kind: Editing,
    title: Entity<InputState>,
    body: Entity<TextareaState>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone)]
enum Row {
    Description,
    EmptyActivity,
    ResolvedThreads,
    Heading(&'static str),
    Comment(usize),
    Review(usize),
    Thread(usize),
    Reply(usize, usize),
}

#[derive(Clone)]
pub(crate) enum GitFeatureEvent {
    OpenFile { repo: String, path: String },
}

impl EventEmitter<GitFeatureEvent> for GitFeature {}

pub(crate) struct GitFeature {
    workspace: Workspace,
    selected: usize,
    details: Option<api::Details>,
    loading: bool,
    loaded: bool,
    error: Option<String>,
    save_error: Option<String>,
    archiving: bool,
    posting: bool,
    generation: u64,
    refreshed: Option<Instant>,
    focus: FocusHandle,
    composer: Option<Composer>,
    full_text: Option<Entity<EditorState>>,
    drafts: HashMap<String, Draft>,
    store_blocked: bool,
    save_generation: u64,
    saved_generation: u64,
    #[cfg(test)]
    save_delay: Duration,
    #[cfg(test)]
    save_started: u64,
    save_task: Option<Task<()>>,
    rows: Vec<Row>,
    markdown: HashMap<String, Entity<TextViewState>>,
    markdown_order: VecDeque<String>,
    list: ListState,
    show_resolved: bool,
    show_drafts: bool,
}

impl GitFeature {
    pub(crate) fn new(workspace: Workspace, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let (drafts, save_error, store_blocked) = match std::fs::read(store_path(&workspace.name)) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(drafts) => (drafts, None, false),
                Err(e) => (
                    HashMap::new(),
                    Some(format!(
                        "Cannot read Git drafts: {e}. The original file is protected."
                    )),
                    true,
                ),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (HashMap::new(), None, false),
            Err(e) => (
                HashMap::new(),
                Some(format!("Cannot read Git drafts: {e}")),
                true,
            ),
        };
        Self {
            workspace,
            selected: 0,
            details: None,
            loading: false,
            loaded: false,
            error: None,
            save_error,
            archiving: false,
            posting: false,
            generation: 0,
            refreshed: None,
            focus: cx.focus_handle(),
            composer: None,
            full_text: None,
            drafts,
            store_blocked,
            save_generation: 0,
            saved_generation: 0,
            #[cfg(test)]
            save_delay: Duration::ZERO,
            #[cfg(test)]
            save_started: 0,
            save_task: None,
            rows: Vec::new(),
            markdown: HashMap::new(),
            markdown_order: VecDeque::new(),
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            show_resolved: false,
            show_drafts: false,
        }
    }

    pub(crate) fn set_workspace(
        &mut self,
        workspace: Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let identity = |ws: &Workspace| {
            ws.repos
                .iter()
                .map(|r| (r.name.clone(), r.src.clone(), ws.branch_of(r).to_string()))
                .collect::<Vec<_>>()
        };
        let changed = identity(&self.workspace) != identity(&workspace);
        self.workspace = workspace;
        if changed {
            self.selected = self
                .selected
                .min(self.workspace.repos.len().saturating_sub(1));
            self.clear_selection(cx);
            self.refresh(window, cx);
        }
    }

    pub(crate) fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        if !self.loaded
            || self
                .refreshed
                .is_some_and(|t| t.elapsed() > Duration::from_secs(60))
        {
            self.refresh(window, cx);
        }
    }

    pub(crate) fn has_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
    }

    pub(crate) fn storage_error(&self) -> Option<&str> {
        self.save_error.as_deref()
    }

    pub(crate) fn set_archiving(&mut self, archiving: bool, cx: &mut Context<Self>) {
        self.archiving = archiving;
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn inspect(&self) -> TestState<'_> {
        TestState {
            selected: self.selected,
            details: &self.details,
            loading: self.loading,
            error: &self.error,
            save_error: &self.save_error,
            archiving: self.archiving,
        }
    }

    #[cfg(test)]
    pub(crate) fn inject_load_state(
        &mut self,
        loading: bool,
        clear: bool,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if clear {
            self.details = None;
        }
        self.loading = loading;
        self.error = error;
        cx.notify();
    }

    pub(crate) fn busy(&self) -> bool {
        self.posting
    }

    fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.capture_draft(cx);
        self.composer = None;
        self.generation += 1;
        self.loaded = false;
        self.details = None;
        self.error = None;
        self.refreshed = None;
        self.rebuild_rows();
        cx.notify();
    }

    pub(crate) fn select_repo(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.posting || index >= self.workspace.repos.len() || self.selected == index {
            return;
        }
        self.clear_selection(cx);
        self.selected = index;
        self.refresh(window, cx);
    }

    pub(crate) fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading || self.posting || self.archiving {
            return;
        }
        let Some(repo) = self.workspace.repos.get(self.selected) else {
            return;
        };
        let wt = self.workspace.dir().join(&repo.name);
        let branch = self.workspace.branch_of(repo).to_string();
        self.generation += 1;
        let generation = self.generation;
        self.loading = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    api::discover(&wt, &branch)?
                        .map(|target| api::load(&wt, &target))
                        .transpose()
                })
                .await;
            this.update_in(cx, |s, window, cx| {
                s.loading = false;
                if s.generation != generation {
                    // Coalesce rapid repo switches into one read of the latest selection.
                    if !s.loaded {
                        s.refresh(window, cx);
                    }
                    cx.notify();
                    return;
                }
                s.loaded = true;
                match result {
                    Ok(details) => {
                        let target_changed = s.details.as_ref().map(|d| &d.target)
                            != details.as_ref().map(|d| &d.target);
                        if target_changed {
                            s.rows.clear();
                            s.capture_draft(cx);
                            s.composer = None;
                        }
                        s.details = details;
                        s.refreshed = Some(Instant::now());
                        s.rebuild_rows();
                    }
                    Err(e) => s.error = Some(e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn rebuild_rows(&mut self) {
        let offset = self.list.logical_scroll_top();
        let had_rows = !self.rows.is_empty();
        self.full_text = None;
        self.rows.clear();
        self.markdown.clear();
        self.markdown_order.clear();
        if let Some(d) = &self.details {
            self.rows.push(Row::Description);
            if !d.comments.is_empty() {
                self.rows.push(Row::Heading("Conversation"));
            }
            self.rows.extend((0..d.comments.len()).map(Row::Comment));
            if !d.reviews.is_empty() {
                self.rows.push(Row::Heading("Reviews"));
            }
            self.rows.extend((0..d.reviews.len()).map(Row::Review));
            if d.comments.is_empty() && d.reviews.is_empty() && d.threads.is_empty() {
                self.rows.push(Row::EmptyActivity);
            }
            if !d.threads.is_empty() {
                self.rows.push(Row::Heading("Review threads"));
                if !self.show_resolved && d.threads.iter().all(|t| t.resolved) {
                    self.rows.push(Row::ResolvedThreads);
                }
            }
            for (i, t) in d
                .threads
                .iter()
                .enumerate()
                .filter(|(_, t)| !t.resolved || self.show_resolved)
            {
                self.rows.push(Row::Thread(i));
                self.rows
                    .extend((0..t.comments.len()).map(|j| Row::Reply(i, j)));
            }
        }
        self.list.reset(self.rows.len());
        if had_rows && !self.rows.is_empty() {
            self.list.scroll_to(ListOffset {
                item_ix: offset.item_ix.min(self.rows.len() - 1),
                ..offset
            });
        }
    }

    pub(crate) fn start_description(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .details
            .as_ref()
            .is_some_and(|d| d.can_update || self.has_draft(&d.target.url(), &Editing::Description))
        {
            self.start_editor(Editing::Description, window, cx);
        }
    }

    pub(crate) fn start_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .details
            .as_ref()
            .is_some_and(|d| d.can_comment || self.has_draft(&d.target.url(), &Editing::Comment))
        {
            self.start_editor(Editing::Comment, window, cx);
        }
    }

    pub(crate) fn start_reply(
        &mut self,
        thread: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.details.as_ref().is_some_and(|d| {
            d.threads.iter().any(|t| t.id == thread && t.can_reply)
                || self.has_draft(&d.target.url(), &Editing::Reply(thread.to_string()))
        }) {
            self.start_editor(Editing::Reply(thread.to_string()), window, cx);
        }
    }

    fn has_draft(&self, target: &str, kind: &Editing) -> bool {
        self.drafts.get(target).is_some_and(|draft| match kind {
            Editing::Description => draft.description.is_some(),
            Editing::Comment => !draft.note.is_empty(),
            Editing::Reply(id) => draft.replies.get(id).is_some_and(|body| !body.is_empty()),
        })
    }

    fn local_drafts(&self) -> Vec<(String, Editing)> {
        let mut targets: Vec<_> = self.drafts.keys().collect();
        targets.sort();
        let mut entries = Vec::new();
        for target in targets {
            let draft = &self.drafts[target];
            let mut kinds = vec![Editing::Description, Editing::Comment];
            let mut replies: Vec<_> = draft.replies.keys().cloned().collect();
            replies.sort();
            kinds.extend(replies.into_iter().map(Editing::Reply));
            entries.extend(
                kinds
                    .into_iter()
                    .filter(|kind| self.has_draft(target, kind))
                    .map(|kind| (target.clone(), kind)),
            );
        }
        entries
    }

    fn can_submit(&self) -> bool {
        let (Some(c), Some(d)) = (&self.composer, &self.details) else {
            return false;
        };
        c.target == d.target.url()
            && match &c.kind {
                Editing::Description => d.can_update,
                Editing::Comment => d.can_comment,
                Editing::Reply(id) => d.threads.iter().any(|t| t.id == *id && t.can_reply),
            }
    }

    fn start_editor(&mut self, kind: Editing, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = &self.details else {
            return;
        };
        let target = d.target.url();
        self.show_drafts = false;
        self.open_draft(target, kind, window, cx);
    }

    fn open_draft(
        &mut self,
        target: String,
        kind: Editing,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.posting || self.archiving {
            return;
        }
        self.full_text = None;
        self.capture_draft(cx);
        let details = self.details.as_ref().filter(|d| d.target.url() == target);
        if !self.has_draft(&target, &kind) && details.is_none() {
            return;
        }
        let draft = self.drafts.entry(target.clone()).or_default();
        let before = draft.clone();
        let (title, body) = match &kind {
            Editing::Description => {
                if draft.description.is_none() {
                    let Some(d) = details else { return };
                    draft.description = Some(DescriptionDraft {
                        original_title: d.title.clone(),
                        original_body: d.body.clone(),
                        title: d.title.clone(),
                        body: d.body.clone(),
                    });
                }
                let edit = draft.description.as_ref().unwrap();
                (edit.title.clone(), edit.body.clone())
            }
            Editing::Comment => (String::new(), draft.note.clone()),
            Editing::Reply(id) => (
                String::new(),
                draft.replies.get(id).cloned().unwrap_or_default(),
            ),
        };
        let draft_changed = *draft != before;
        let title = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(title)
                .placeholder("PR title")
        });
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 8)
                .default_value(body)
                .placeholder("Write Markdown…")
        });
        let title_sub = cx.subscribe(&title, |s, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                s.capture_draft(cx);
            }
        });
        let body_sub = cx.subscribe(&body, |s, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                s.capture_draft(cx);
            }
        });
        body.read(cx).focus_handle(cx).focus(window, cx);
        self.composer = Some(Composer {
            target,
            kind,
            title,
            body,
            _subscriptions: vec![title_sub, body_sub],
        });
        if draft_changed {
            self.save_generation += 1;
            self.schedule_save(cx);
        }
        cx.notify();
    }

    fn capture_draft(&mut self, cx: &mut Context<Self>) {
        let Some(c) = &self.composer else {
            return;
        };
        let draft = self.drafts.entry(c.target.clone()).or_default();
        let before = draft.clone();
        let body = c.body.read(cx).value().to_string();
        match &c.kind {
            Editing::Description => {
                if let Some(d) = &mut draft.description {
                    d.title = c.title.read(cx).value().to_string();
                    d.body = body;
                }
            }
            Editing::Comment => draft.note = body,
            Editing::Reply(id) => {
                draft.replies.insert(id.clone(), body);
            }
        }
        if *draft != before {
            self.save_generation += 1;
            self.schedule_save(cx);
            cx.notify();
        }
    }

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        if self.store_blocked
            || self.save_task.is_some()
            || (self.saved_generation == self.save_generation && self.save_error.is_none())
        {
            return;
        }
        // One worker owns write ordering; input only updates the next snapshot.
        self.save_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(300))
                    .await;
                let Ok((generation, path, bytes)) = this.update(cx, |s, _| {
                    #[cfg(test)]
                    {
                        s.save_started = s.save_generation;
                    }
                    (
                        s.save_generation,
                        store_path(&s.workspace.name),
                        serde_json::to_vec(&s.drafts).expect("Git drafts serialize"),
                    )
                }) else {
                    return;
                };
                #[cfg(test)]
                {
                    let delay = this
                        .update(cx, |s, _| std::mem::take(&mut s.save_delay))
                        .unwrap_or_default();
                    cx.background_executor().timer(delay).await;
                }
                let result = cx
                    .background_executor()
                    .spawn(async move { crate::state::write(&path, &bytes) })
                    .await;
                let more = this
                    .update(cx, |s, cx| {
                        if result.is_ok() {
                            s.saved_generation = generation;
                        }
                        let more = s.save_generation != generation;
                        if !more {
                            s.save_error = result.err().map(|e| e.to_string());
                            s.save_task = None;
                        }
                        cx.notify();
                        more
                    })
                    .unwrap_or(false);
                if !more {
                    break;
                }
            }
        }));
    }

    pub(crate) fn saving_drafts(&self) -> bool {
        self.save_task.is_some()
    }

    pub(crate) fn retry_save(&mut self, cx: &mut Context<Self>) {
        self.capture_draft(cx);
        self.schedule_save(cx);
        cx.notify();
    }

    fn close_editor(&mut self, cx: &mut Context<Self>) {
        self.capture_draft(cx);
        self.composer = None;
        cx.notify();
    }

    fn confirm_discard_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = &self.composer else {
            return;
        };
        let (target, kind) = (c.target.clone(), c.kind.clone());
        confirm(
            "Discard Git draft?".into(),
            "This removes the local draft. GitHub stays unchanged.",
            "Discard draft",
            move |s: &mut Self, w, cx| {
                if s.composer
                    .as_ref()
                    .is_some_and(|c| c.target == target && c.kind == kind)
                {
                    s.discard_editor(cx);
                    s.focus.focus(w, cx);
                }
            },
            window,
            cx,
        );
    }

    fn discard_editor(&mut self, cx: &mut Context<Self>) {
        if self.posting {
            return;
        }
        if let Some(c) = self.composer.take() {
            if let Some(d) = self.drafts.get_mut(&c.target) {
                match c.kind {
                    Editing::Description => d.description = None,
                    Editing::Comment => d.note.clear(),
                    Editing::Reply(id) => {
                        d.replies.remove(&id);
                    }
                }
            }
            self.save_generation += 1;
            self.schedule_save(cx);
        }
        cx.notify();
    }

    fn description_changed(&self) -> bool {
        let (Some(c), Some(d)) = (&self.composer, &self.details) else {
            return false;
        };
        c.kind == Editing::Description
            && c.target == d.target.url()
            && self
                .drafts
                .get(&c.target)
                .and_then(|draft| draft.description.as_ref())
                .is_some_and(|draft| {
                    draft.original_title != d.title || draft.original_body != d.body
                })
    }

    fn replace_description(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.posting
            || self.loading
            || self.archiving
            || !self.can_submit()
            || !self.description_changed()
        {
            return;
        }
        let d = self.details.as_ref().unwrap();
        let (target, title, body) = (d.target.url(), d.title.clone(), d.body.clone());
        confirm(
            "Replace the updated description?".into(),
            "This replaces the latest PR title and description shown above with your draft. If GitHub changes again, saving stops.",
            "Replace description",
            move |s: &mut Self, w, cx| {
                if s.posting || s.loading || s.archiving {
                    return;
                }
                if !s
                    .composer
                    .as_ref()
                    .is_some_and(|c| c.target == target && c.kind == Editing::Description)
                {
                    return;
                }
                if !s
                    .details
                    .as_ref()
                    .is_some_and(|d| d.target.url() == target && d.title == title && d.body == body)
                {
                    s.error = Some(
                        "The description changed again. Refresh and compare before replacing."
                            .into(),
                    );
                    cx.notify();
                    return;
                }
                if let Some(draft) = s
                    .drafts
                    .get_mut(&target)
                    .and_then(|draft| draft.description.as_mut())
                {
                    draft.original_title = title.clone();
                    draft.original_body = body.clone();
                    s.save_generation += 1;
                    s.schedule_save(cx);
                }
                s.submit(w, cx);
            },
            window,
            cx,
        );
    }

    pub(crate) fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.full_text.is_some() || !self.can_submit() {
            return;
        }
        if self.posting || self.loading || self.archiving {
            return;
        }
        self.capture_draft(cx);
        let (Some(c), Some(mut details)) = (&self.composer, self.details.clone()) else {
            return;
        };
        if c.target != details.target.url() {
            return;
        }
        let kind = c.kind.clone();
        let title = c.title.read(cx).value().to_string();
        let body = c.body.read(cx).value().to_string();
        if matches!(kind, Editing::Description) {
            if title.trim().is_empty() {
                self.error = Some("A PR title cannot be empty".into());
                cx.notify();
                return;
            }
            if let Some(d) = self
                .drafts
                .get(&c.target)
                .and_then(|d| d.description.as_ref())
            {
                details.title = d.original_title.clone();
                details.body = d.original_body.clone();
            }
        } else if body.trim().is_empty() {
            return;
        }
        let Some(repo) = self.workspace.repos.get(self.selected) else {
            return;
        };
        let wt = self.workspace.dir().join(&repo.name);
        let target = c.target.clone();
        self.posting = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let sent_kind = kind.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    match &kind {
                        Editing::Description => api::save_description(&wt, &details, &title, &body),
                        Editing::Comment => api::comment(&wt, &details, &body),
                        Editing::Reply(id) => api::reply(&wt, &details, id, &body),
                    }
                })
                .await;
            this.update_in(cx, |s, window, cx| {
                s.posting = false;
                match result {
                    Ok(()) => {
                        if let Some(d) = s.drafts.get_mut(&target) {
                            match sent_kind {
                                Editing::Description => d.description = None,
                                Editing::Comment => d.note.clear(),
                                Editing::Reply(id) => {
                                    d.replies.remove(&id);
                                }
                            }
                        }
                        if s.composer.as_ref().is_some_and(|c| c.target == target) {
                            s.composer = None;
                        }
                        s.save_generation += 1;
                        s.schedule_save(cx);
                        s.refresh(window, cx);
                    }
                    Err(e) => s.error = Some(e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn resolve(
        &mut self,
        id: &str,
        resolved: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.posting || self.loading || self.archiving {
            return;
        }
        let Some(d) = self.details.clone() else {
            return;
        };
        let allowed = d.threads.iter().any(|t| {
            t.id == id
                && if resolved {
                    t.can_resolve
                } else {
                    t.can_unresolve
                }
        });
        if !allowed {
            return;
        }
        let Some(repo) = self.workspace.repos.get(self.selected) else {
            return;
        };
        let wt = self.workspace.dir().join(&repo.name);
        let id = id.to_string();
        self.posting = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { api::resolve(&wt, &d, &id, resolved) })
                .await;
            this.update_in(cx, |s, window, cx| {
                s.posting = false;
                match result {
                    Ok(()) => s.refresh(window, cx),
                    Err(e) => s.error = Some(e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn md(&mut self, id: String, body: &str, cx: &mut Context<Self>) -> Entity<TextViewState> {
        if let Some(at) = self.markdown_order.iter().position(|key| key == &id) {
            self.markdown_order.remove(at);
        }
        self.markdown_order.push_back(id.clone());
        if self.markdown_order.len() > 128
            && let Some(oldest) = self.markdown_order.pop_front()
        {
            self.markdown.remove(&oldest);
        }
        self.markdown
            .entry(id)
            .or_insert_with(|| cx.new(|cx| TextViewState::markdown(markdown_preview(body), cx)))
            .clone()
    }

    fn body_view(&mut self, id: String, body: String, cx: &mut Context<Self>) -> AnyElement {
        let md = self.md(id.clone(), &body, cx);
        v_flex()
            .gap_2()
            .when(body.len() > PREVIEW_BYTES, |d| {
                d.child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .text_xs()
                        .child("Preview of a long message")
                        .child(
                            Button::new(SharedString::from(format!("git-long-{id}")))
                                .label("Read full text")
                                .small()
                                .on_click(cx.listener(move |s, _, w, cx| {
                                    let input = cx.new(|cx| {
                                        EditorState::new(w, cx)
                                            .default_value(body.clone())
                                            .soft_wrap(true)
                                    });
                                    input.update(cx, |i, cx| i.set_readonly(true, cx));
                                    input.read(cx).focus_handle(cx).focus(w, cx);
                                    s.full_text = Some(input);
                                    cx.notify();
                                })),
                        ),
                )
            })
            .child(crate::chat::markdown(&md, "git-row"))
            .into_any_element()
    }

    fn overview(&self, cx: &mut Context<Self>) -> AnyElement {
        let d = self.details.as_ref().unwrap();
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let disabled = self.posting || self.archiving;
        let url = d.url.clone();
        let draft = self.drafts.get(&d.target.url());
        let resume_description = draft
            .and_then(|draft| draft.description.as_ref())
            .is_some_and(|draft| !draft.title.is_empty() || !draft.body.is_empty());
        v_flex()
            .id("git-overview")
            .test_support()
            .gap_3()
            .pt_4()
            .pb_6()
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("Pull request #{}", d.target.number))
                    .child(div().flex_1())
                    .child(
                        Button::new("git-open-pr")
                            .label("Open in GitHub")
                            .icon(IconName::ExternalLink)
                            .ghost()
                            .xsmall()
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    ),
            )
            .child(
                div()
                    .id("git-pr-title")
                    .test_support()
                    .text_size(px(24.))
                    .line_height(relative(1.25))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(d.title.clone()),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .text_sm()
                    .child(
                        h_flex()
                            .gap_1()
                            .px_2()
                            .py_0p5()
                            .rounded(theme.radius)
                            .bg(theme.secondary)
                            .text_color(match d.state.as_str() {
                                "OPEN" if !d.draft => theme.success_foreground,
                                "MERGED" => theme.primary,
                                _ => muted,
                            })
                            .child(
                                Icon::new(match d.state.as_str() {
                                    "MERGED" => IconName::GitMerge,
                                    "CLOSED" => IconName::GitPullRequestClosed,
                                    _ if d.draft => IconName::GitPullRequestDraft,
                                    _ => IconName::GitPullRequest,
                                })
                                .with_size(px(14.)),
                            )
                            .child(state_label(&d.state).to_string()),
                    )
                    .when(d.draft, |r| r.child(div().text_color(muted).child("Draft")))
                    .child(div().text_color(muted).child(format!("by @{}", d.author))),
            )
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(muted)
                    .child(
                        Icon::new(IconName::GitBranch)
                            .with_size(px(14.))
                            .flex_none(),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .font_family(theme.mono_font_family.clone())
                            .child(format!("{} ← {}", d.base, d.head)),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .when(d.can_update || resume_description, |r| {
                        r.child(
                            Button::new("git-description-edit")
                                .icon(IconName::Pencil)
                                .label(if resume_description {
                                    "Resume description"
                                } else {
                                    "Edit description"
                                })
                                .small()
                                .ghost()
                                .disabled(disabled)
                                .on_click(cx.listener(|s, _, w, cx| s.start_description(w, cx))),
                        )
                    })
                    .when(d.threads.iter().any(|t| t.resolved), |r| {
                        r.child(
                            Button::new("git-show-resolved")
                                .label(if self.show_resolved {
                                    "Hide resolved"
                                } else {
                                    "Show resolved"
                                })
                                .small()
                                .ghost()
                                .on_click(cx.listener(|s, _, _, cx| {
                                    s.show_resolved = !s.show_resolved;
                                    s.rebuild_rows();
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .w_full()
            .px_4()
            .child(
                div()
                    .w_full()
                    .max_w(px(680.))
                    .mx_auto()
                    .child(self.render_row_content(index, window, cx)),
            )
            .into_any_element()
    }

    fn render_row_content(
        &mut self,
        index: usize,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row) = self.rows.get(index).cloned() else {
            return div().into_any_element();
        };
        let Some(details) = &self.details else {
            return div().into_any_element();
        };
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let base = v_flex()
            .id(("git-row", index))
            .test_support()
            .group("git-row")
            .w_full()
            .min_w_0()
            .py_3()
            .gap_2()
            .text_size(px(14.))
            .line_height(relative(1.5));
        let comment = match row {
            Row::Description => {
                let body = details.body.clone();
                let empty = body.trim().is_empty();
                return base
                    .child(self.overview(cx))
                    .child(div().text_xs().text_color(muted).child("Description"))
                    .child(if empty {
                        div()
                            .id("git-empty-description")
                            .test_support()
                            .text_color(muted)
                            .child("No description added yet.")
                            .into_any_element()
                    } else {
                        self.body_view("description".into(), body, cx)
                    })
                    .into_any_element();
            }
            Row::EmptyActivity | Row::ResolvedThreads => {
                let resolved = matches!(row, Row::ResolvedThreads);
                return base
                    .id(if resolved {
                        "git-resolved-threads"
                    } else {
                        "git-empty-activity"
                    })
                    .test_support()
                    .py_6()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Icon::new(if resolved {
                                    IconName::CircleCheck
                                } else {
                                    IconName::MessageSquare
                                })
                                .with_size(px(16.))
                                .text_color(muted),
                            )
                            .child(if resolved {
                                "All review threads resolved"
                            } else {
                                "No activity yet"
                            }),
                    )
                    .child(div().text_sm().text_color(muted).child(if resolved {
                        "Show resolved threads to revisit earlier discussions."
                    } else if details.can_comment {
                        "Start the conversation with Add comment."
                    } else {
                        "Comments and reviews will appear here."
                    }))
                    .when(resolved, |d| {
                        d.child(
                            h_flex().child(
                                Button::new("git-resolved-show")
                                    .label("Show resolved")
                                    .small()
                                    .ghost()
                                    .on_click(cx.listener(|s, _, _, cx| {
                                        s.show_resolved = true;
                                        s.rebuild_rows();
                                        cx.notify();
                                    })),
                            ),
                        )
                    })
                    .into_any_element();
            }
            Row::Heading(label) => {
                return base
                    .pt_6()
                    .pb_2()
                    .text_xs()
                    .text_color(muted)
                    .font_weight(FontWeight::MEDIUM)
                    .child(label)
                    .into_any_element();
            }
            Row::Comment(i) => (details.comments[i].clone(), None),
            Row::Review(i) => (
                details.reviews[i].comment.clone(),
                Some(details.reviews[i].state.clone()),
            ),
            Row::Reply(i, j) => (details.threads[i].comments[j].clone(), None),
            Row::Thread(i) => {
                let t = &details.threads[i];
                let resume = self
                    .drafts
                    .get(&details.target.url())
                    .and_then(|d| d.replies.get(&t.id))
                    .is_some_and(|body| !body.is_empty());
                let (id, resolve_id, resolved) = (t.id.clone(), t.id.clone(), t.resolved);
                let repo = self.workspace.repos[self.selected].name.clone();
                let path = t.path.clone();
                let place = format!(
                    "{}{}{}{}",
                    t.path,
                    t.line.map(|l| format!(":{l}")).unwrap_or_default(),
                    if t.outdated { " · Outdated" } else { "" },
                    if t.resolved {
                        " · Resolved"
                    } else {
                        " · Unresolved"
                    }
                );
                return base
                    .mt_3()
                    .px_3()
                    .bg(cx.theme().secondary)
                    .rounded_t(cx.theme().radius_lg)
                    .border_1()
                    .border_color(border)
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                Button::new(("git-thread-path", i))
                                    .tooltip(format!("Show file in workspace: {place}"))
                                    .accessibility_label(place.clone())
                                    .icon(IconName::FileCode)
                                    .child(div().flex_1().min_w_0().truncate().child(place))
                                    .px_0()
                                    .flex_1()
                                    .min_w_0()
                                    .ghost()
                                    .small()
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.emit(GitFeatureEvent::OpenFile {
                                            repo: repo.clone(),
                                            path: path.clone(),
                                        })
                                    })),
                            )
                            .when(t.can_reply || resume, |d| {
                                d.child(
                                    Button::new(SharedString::from(format!(
                                        "git-thread-reply-{}",
                                        t.id
                                    )))
                                    .label(if resume { "Resume reply" } else { "Reply" })
                                    .icon(IconName::Reply)
                                    .ghost()
                                    .small()
                                    .disabled(self.posting || self.archiving)
                                    .on_click(
                                        cx.listener(move |s, _, w, cx| s.start_reply(&id, w, cx)),
                                    ),
                                )
                            })
                            .when(
                                if resolved {
                                    t.can_unresolve
                                } else {
                                    t.can_resolve
                                },
                                |d| {
                                    d.child(
                                        Button::new(SharedString::from(format!(
                                            "git-thread-resolve-{}",
                                            t.id
                                        )))
                                        .label(if resolved { "Reopen" } else { "Resolve" })
                                        .icon(if resolved {
                                            IconName::RotateCcw
                                        } else {
                                            IconName::Check
                                        })
                                        .ghost()
                                        .small()
                                        .disabled(self.posting || self.loading || self.archiving)
                                        .on_click(
                                            cx.listener(move |s, _, w, cx| {
                                                s.resolve(&resolve_id, !resolved, w, cx)
                                            }),
                                        ),
                                    )
                                },
                            ),
                    )
                    .into_any_element();
            }
        };
        let (c, state) = comment;
        let body_view = self.body_view(c.id.clone(), c.body.clone(), cx);
        let url = c.url.clone();
        base.when(matches!(row, Row::Reply(..)), |d| {
            d.px_3()
                .border_l_1()
                .border_r_1()
                .border_b_1()
                .border_color(border)
                .bg(cx.theme().secondary)
                .when(matches!(row, Row::Reply(i, j) if j + 1 == self.details.as_ref().unwrap().threads[i].comments.len()), |d| {
                    d.rounded_b(cx.theme().radius_lg)
                })
        })
        .when(matches!(row, Row::Review(..)), |d| {
            d.p_3()
                .mt_2()
                .rounded(cx.theme().radius_lg)
                .border_1()
                .border_color(border)
                .bg(cx.theme().secondary)
        })
        .child(
            h_flex()
                .gap_2()
                .items_start()
                .text_xs()
                .text_color(muted)
                .child(
                    h_flex().flex_1().min_w_0().gap_2().flex_wrap()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(cx.theme().foreground)
                        .child(if c.author.is_empty() {
                            "Deleted user".to_string()
                        } else {
                            format!("@{}", c.author)
                        }),
                )
                .when_some(state, |d, state| {
                    d.child(
                        h_flex()
                            .gap_1()
                            .px_2()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().muted)
                            .text_color(match state.as_str() {
                                "APPROVED" => cx.theme().success_foreground,
                                "CHANGES_REQUESTED" => cx.theme().warning_foreground,
                                _ => muted,
                            })
                            .child(
                                Icon::new(match state.as_str() {
                                    "APPROVED" => IconName::CircleCheck,
                                    "CHANGES_REQUESTED" => IconName::CircleAlert,
                                    "PENDING" => IconName::Clock,
                                    "DISMISSED" => IconName::CircleMinus,
                                    _ => IconName::MessageSquare,
                                })
                                .with_size(px(12.)),
                            )
                            .child(state_label(&state).to_string()),
                    )
                })
                .child(c.created_at.replace('T', " · ").replace('Z', " UTC"))
                )
                .when(!url.is_empty(), |d| {
                    d.child(
                        Button::new(("git-comment-link", index))
                            .label("GitHub")
                            .icon(IconName::ExternalLink)
                            .ghost()
                            .xsmall()
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                }),
        )
        .child(body_view)
        .into_any_element()
    }

    fn scroll(&self, lines: f32, cx: &mut Context<Self>) {
        self.list.scroll_by(px(lines * 21.));
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn delay_next_save(&mut self, delay: Duration) {
        self.save_delay = delay;
    }

    #[cfg(test)]
    pub(crate) fn save_started(&self) -> u64 {
        self.save_started
    }

    #[cfg(test)]
    pub(crate) fn inject_details(&mut self, details: api::Details, cx: &mut Context<Self>) {
        self.generation += 1;
        self.loading = false;
        self.loaded = true;
        self.refreshed = Some(Instant::now());
        self.details = Some(details);
        self.rebuild_rows();
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn full_text_len(&self, cx: &App) -> usize {
        self.full_text
            .as_ref()
            .map(|input| input.read(cx).value().len())
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub(crate) fn scroll_offset(&self) -> ListOffset {
        self.list.logical_scroll_top()
    }

    #[cfg(test)]
    pub(crate) fn markdown_text_len(&self, key: &str, cx: &App) -> usize {
        self.markdown
            .get(key)
            .map(|m| m.read(cx).rendered_text().as_str().len())
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub(crate) fn scroll_row(&mut self, index: usize, cx: &mut Context<Self>) {
        self.list.scroll_to(ListOffset {
            item_ix: index,
            offset_in_item: px(0.),
        });
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn draft_text(&self, cx: &App) -> Option<String> {
        self.composer
            .as_ref()
            .map(|c| c.body.read(cx).value().to_string())
    }
}

const PREVIEW_BYTES: usize = 4096;

// shortcut: rich text lays out whole paragraphs; remove this preview when the renderer virtualizes them.
fn markdown_preview(body: &str) -> &str {
    if body.len() <= PREVIEW_BYTES {
        return body;
    }
    let mut end = PREVIEW_BYTES;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    &body[..end]
}

fn state_label(state: &str) -> &str {
    match state {
        "OPEN" => "Open",
        "CLOSED" => "Closed",
        "MERGED" => "Merged",
        "APPROVED" => "Approved",
        "CHANGES_REQUESTED" => "Changes requested",
        "COMMENTED" => "Commented",
        "DISMISSED" => "Dismissed",
        "PENDING" => "Pending",
        _ => state,
    }
}

fn store_path(workspace: &str) -> PathBuf {
    crate::state::support_dir()
        .join("git-drafts")
        .join(format!("{workspace}.json"))
}

impl Render for GitFeature {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, muted) = (theme.border, theme.muted_foreground);
        let disabled = self.posting || self.archiving;
        let repo = self.workspace.repos.get(self.selected);
        let drafts = self.local_drafts();
        let toolbar = h_flex()
            .flex_none()
            .gap_2()
            .px_4()
            .py_2()
            .flex_wrap()
            .border_b_1()
            .border_color(border)
            .children(self.workspace.repos.iter().enumerate().map(|(i, r)| {
                Button::new(SharedString::from(format!("git-repo-{i}")))
                    .label(r.name.clone())
                    .small()
                    .ghost()
                    .selected(i == self.selected)
                    .disabled(disabled)
                    .on_click(cx.listener(move |s, _, w, cx| s.select_repo(i, w, cx)))
            }))
            .child(div().flex_1())
            .when(!drafts.is_empty() || self.show_drafts, |r| {
                r.child(
                    Button::new("git-local-drafts")
                        .label(if self.show_drafts {
                            "Conversation".to_string()
                        } else {
                            format!("Local drafts ({})", drafts.len())
                        })
                        .small()
                        .ghost()
                        .disabled(disabled)
                        .on_click(cx.listener(|s, _, w, cx| {
                            s.show_drafts = !s.show_drafts;
                            s.full_text = None;
                            s.focus.focus(w, cx);
                            cx.notify();
                        })),
                )
            })
            .when_some(self.details.as_ref(), |r, d| {
                let resume_comment = self
                    .drafts
                    .get(&d.target.url())
                    .is_some_and(|draft| !draft.note.is_empty());
                r.when(d.can_comment || resume_comment, |r| {
                    r.child(
                        Button::new("git-comment-new")
                            .icon(IconName::MessageSquare)
                            .label(if resume_comment {
                                "Resume comment"
                            } else {
                                "Add comment"
                            })
                            .small()
                            .ghost()
                            .disabled(disabled)
                            .on_click(cx.listener(|s, _, w, cx| s.start_comment(w, cx))),
                    )
                })
            })
            .child(
                Button::new("git-refresh")
                    .icon(IconName::RefreshCw)
                    .loading(self.loading)
                    .label(if self.loading {
                        "Refreshing…"
                    } else {
                        "Refresh"
                    })
                    .small()
                    .ghost()
                    .disabled(disabled || self.loading || repo.is_none())
                    .on_click(cx.listener(|s, _, w, cx| s.refresh(w, cx))),
            );
        let (state_id, state_title, state_detail, state_icon) = if repo.is_none() {
            (
                "git-empty-repositories",
                "No repositories in this workspace",
                "Add a repository to the workspace to view its pull request.",
                IconName::FolderGit2,
            )
        } else if self.loading {
            (
                "git-loading",
                "Loading pull request…",
                "Fetching the description and review activity from GitHub.",
                IconName::GitPullRequest,
            )
        } else if self.error.is_some() {
            (
                "git-load-error",
                "Could not load the pull request",
                "Check the connection and GitHub access, then try again.",
                IconName::TriangleAlert,
            )
        } else if !self.loaded {
            (
                "git-unloaded",
                "Pull request not loaded",
                if self.archiving {
                    "This workspace is being archived."
                } else {
                    "Load this branch’s pull request to read its description and review activity."
                },
                IconName::GitPullRequest,
            )
        } else {
            (
                "git-empty-pr",
                "No pull request yet",
                "Use Create PRs in the command menu (⌘K), then refresh to see the conversation here.",
                IconName::GitPullRequest,
            )
        };
        let content = if self.show_drafts {
            v_flex()
                .id("git-local-drafts-list")
                .test_support()
                .size_full()
                .overflow_y_scroll()
                .p_4()
                .gap_3()
                .child(
                    div()
                        .text_sm()
                        .child("Saved on this computer. Open a draft to edit, copy or discard it."),
                )
                .when(drafts.is_empty(), |d| d.child("No local drafts"))
                .children(drafts.into_iter().enumerate().map(|(i, (target, kind))| {
                    let label = match &kind {
                        Editing::Description => "Resume description".to_string(),
                        Editing::Comment => "Resume comment".to_string(),
                        Editing::Reply(_) => "Resume reply".to_string(),
                    };
                    let context = match &kind {
                        Editing::Reply(id) => self
                            .details
                            .as_ref()
                            .filter(|d| d.target.url() == target)
                            .and_then(|d| d.threads.iter().find(|t| t.id == *id))
                            .map(|t| t.path.clone())
                            .or_else(|| {
                                self.drafts
                                    .get(&target)?
                                    .replies
                                    .get(id)
                                    .map(|body| body.chars().take(100).collect::<String>())
                            }),
                        _ => None,
                    };
                    v_flex()
                        .gap_1()
                        .child(div().text_xs().text_color(muted).child(target.clone()))
                        .when_some(context, |d, context| {
                            d.child(div().text_sm().child(context))
                        })
                        .child(
                            h_flex().child(
                                Button::new(("git-local-draft", i))
                                    .label(label)
                                    .small()
                                    .disabled(disabled)
                                    .on_click(cx.listener(move |s, _, w, cx| {
                                        s.open_draft(target.clone(), kind.clone(), w, cx)
                                    })),
                            ),
                        )
                }))
                .into_any_element()
        } else if let Some(input) = &self.full_text {
            v_flex()
                .size_full()
                .min_h_0()
                .child(
                    h_flex()
                        .px_4()
                        .py_2()
                        .gap_2()
                        .flex_none()
                        .child(
                            div()
                                .flex_1()
                                .text_sm()
                                .child("Full Markdown text · read only · ⌘F to search"),
                        )
                        .child(
                            Button::new("git-full-close")
                                .icon(IconName::ArrowLeft)
                                .label("Back to conversation")
                                .small()
                                .on_click(cx.listener(|s, _, w, cx| {
                                    s.full_text = None;
                                    s.focus.focus(w, cx);
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                    div()
                        .id("git-full-text")
                        .test_support()
                        .flex_1()
                        .min_h_0()
                        .child(Editor::new(input).readonly(true).appearance(false).h_full()),
                )
                .into_any_element()
        } else if self.details.is_some() {
            list(
                self.list.clone(),
                cx.processor(|s, i, w, cx| s.render_row(i, w, cx)),
            )
            .size_full()
            .py_2()
            .into_any_element()
        } else {
            v_flex()
                .id(state_id)
                .test_support()
                .aria_label(state_title)
                .size_full()
                .min_h_0()
                .overflow_y_scroll()
                .px_4()
                .py_8()
                .child(
                    v_flex()
                        .w_full()
                        .max_w(px(440.))
                        .mx_auto()
                        .items_center()
                        .gap_3()
                        .text_center()
                        .text_size(px(14.))
                        .line_height(relative(1.5))
                        .child(if self.loading && repo.is_some() {
                            Spinner::new()
                                .with_size(px(28.))
                                .color(muted)
                                .into_any_element()
                        } else {
                            Icon::new(state_icon)
                                .with_size(px(28.))
                                .text_color(if self.error.is_some() {
                                    theme.warning_foreground
                                } else {
                                    muted
                                })
                                .into_any_element()
                        })
                        .child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::MEDIUM)
                                .child(state_title),
                        )
                        .child(div().text_color(muted).child(state_detail))
                        .when_some(self.error.as_ref().filter(|_| repo.is_some()), |d, e| {
                            d.child(
                                div()
                                    .id("git-error")
                                    .test_support()
                                    .w_full()
                                    .max_h(px(120.))
                                    .overflow_y_scroll()
                                    .p_3()
                                    .rounded(theme.radius)
                                    .bg(theme.secondary)
                                    .text_sm()
                                    .text_left()
                                    .text_color(muted)
                                    .child(e.clone()),
                            )
                        })
                        .when(!self.loading && repo.is_some(), |d| {
                            d.child(
                                Button::new("git-state-retry")
                                    .icon(IconName::RefreshCw)
                                    .label(if self.error.is_some() {
                                        "Try again"
                                    } else if !self.loaded {
                                        "Load pull request"
                                    } else {
                                        "Refresh"
                                    })
                                    .small()
                                    .disabled(disabled)
                                    .on_click(cx.listener(|s, _, w, cx| s.refresh(w, cx))),
                            )
                        }),
                )
                .into_any_element()
        };
        let composer = self.composer.as_ref().filter(|_| self.full_text.is_none()).map(|c| {
            let title = match &c.kind {
                Editing::Description => "Edit PR description",
                Editing::Comment => "Comment on GitHub",
                Editing::Reply(_) => "Reply to review thread",
            };
            v_flex()
                .id("git-composer")
                .test_support()
                .w_full()
                .max_w(px(680.))
                .mx_auto()
                .p_3()
                .gap_2()
                .border_1()
                .border_color(border)
                .rounded(theme.radius_lg)
                .bg(theme.secondary)
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(div().text_xs().text_color(muted).child(c.target.clone()))
                .when(!self.can_submit(), |d| d.child(div().text_sm().child("Saved locally. Load this PR with permission to post before sending to GitHub.")))
                .when(self.description_changed(), |d| d.child(
                    v_flex().gap_2().text_sm()
                        .child("GitHub’s description changed. Compare the latest version above with your draft.")
                        .child(Button::new("git-description-replace").label("Replace with my draft…").small()
                            .disabled(disabled || self.loading)
                            .on_click(cx.listener(|s,_,w,cx| s.replace_description(w,cx))))))
                .when(c.kind == Editing::Description, |d| {
                    d.child(
                        div()
                            .id("git-editor-title")
                            .test_support()
                            .child(Input::new(&c.title).disabled(disabled)),
                    )
                })
                .child(
                    div()
                        .id("git-editor-body")
                        .test_support()
                        .child(Textarea::new(&c.body).disabled(disabled)),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .child(
                            Button::new("git-editor-save")
                                .icon(if c.kind == Editing::Description { IconName::Check } else { IconName::Send })
                                .loading(self.posting)
                                .label(if self.posting {
                                    "Saving…"
                                } else if c.kind == Editing::Description {
                                    "Save to GitHub"
                                } else {
                                    "Post to GitHub"
                                })
                                .primary()
                                .small()
                                .disabled(disabled || self.loading || !self.can_submit())
                                .on_click(cx.listener(|s, _, w, cx| s.submit(w, cx))),
                        )
                        .child(
                            Button::new("git-editor-cancel")
                                .label("Keep draft")
                                .small()
                                .disabled(disabled)
                                .on_click(cx.listener(|s, _, w, cx| {
                                    s.close_editor(cx);
                                    s.focus.focus(w, cx);
                                })),
                        )
                        .child(
                            Button::new("git-editor-discard")
                                .label("Discard draft")
                                .ghost()
                                .small()
                                .disabled(disabled)
                                .on_click(
                                    cx.listener(|s, _, w, cx| s.confirm_discard_editor(w, cx)),
                                ),
                        ),
                )
        });
        v_flex()
            .id("git-feature")
            .key_context("GitFeature")
            .on_action(cx.listener(|s, _: &SaveFile, w, cx| s.submit(w, cx)))
            .on_action(cx.listener(|s, _: &SelectNext, _, cx| s.scroll(2., cx)))
            .on_action(cx.listener(|s, _: &SelectPrev, _, cx| s.scroll(-2., cx)))
            .on_action(cx.listener(|s, _: &PageDown, _, cx| s.scroll(15., cx)))
            .on_action(cx.listener(|s, _: &PageUp, _, cx| s.scroll(-15., cx)))
            .on_action(cx.listener(|s, _: &SelectFirst, _, cx| {
                s.list.scroll_to(ListOffset::default());
                cx.notify();
            }))
            .on_action(cx.listener(|s, _: &SelectLast, _, cx| {
                s.list.scroll_to_end();
                cx.notify();
            }))
            .on_action(cx.listener(|s, _: &CancelEdit, w, cx| {
                if !s.posting {
                    if s.full_text.take().is_none() {
                        s.close_editor(cx);
                    }
                    s.focus.focus(w, cx);
                }
            }))
            .track_focus(&self.focus)
            .aria_label("Git pull request")
            .size_full()
            .min_h_0()
            .min_w_0()
            .child(toolbar)
            .when_some(self.save_error.clone(), |d, e| {
                d.child(
                    h_flex()
                        .px_4()
                        .py_2()
                        .gap_2()
                        .items_start()
                        .text_sm()
                        .child(
                            Icon::new(IconName::TriangleAlert)
                                .with_size(px(16.))
                                .text_color(theme.warning_foreground)
                                .flex_none(),
                        )
                        .child(div().min_w_0().child(format!("Draft not saved: {e}"))),
                )
            })
            .when_some(
                self.error.clone().filter(|_| self.details.is_some()),
                |d, e| {
                    d.child(
                        h_flex()
                            .id("git-error")
                            .test_support()
                            .px_4()
                            .py_2()
                            .gap_2()
                            .items_start()
                            .text_sm()
                            .child(
                                Icon::new(IconName::TriangleAlert)
                                    .with_size(px(16.))
                                    .text_color(theme.warning_foreground)
                                    .flex_none(),
                            )
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .gap_1()
                                    .child("Showing previously loaded data")
                                    .child(
                                        div()
                                            .id("git-error-detail")
                                            .max_h(px(120.))
                                            .overflow_y_scroll()
                                            .text_color(muted)
                                            .child(e),
                                    ),
                            ),
                    )
                },
            )
            .child(div().flex_1().min_h_0().child(content))
            .when_some(composer, |d, c| {
                d.child(div().flex_none().w_full().px_4().py_3().child(c))
            })
            .test_support()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[::core::prelude::v1::test]
    fn long_markdown_preview_preserves_unicode_and_source() {
        let body = format!("{}🙂tail", "a".repeat(PREVIEW_BYTES - 1));
        assert_eq!(markdown_preview(&body).len(), PREVIEW_BYTES - 1);
        assert!(body.ends_with("🙂tail"));
        assert_eq!(markdown_preview("short"), "short");
    }

    #[::core::prelude::v1::test]
    fn drafts_preserve_baseline_and_separate_targets_and_threads() {
        let mut drafts = HashMap::<String, Draft>::new();
        let d = drafts
            .entry("https://github.com/o/r/pull/1".into())
            .or_default();
        d.description = Some(DescriptionDraft {
            original_title: "old".into(),
            original_body: "baseline".into(),
            title: "new".into(),
            body: "edited".into(),
        });
        d.replies.insert("thread-a".into(), "first".into());
        d.replies.insert("thread-b".into(), "second".into());
        drafts
            .entry("https://github.com/o/r/pull/2".into())
            .or_default()
            .note = "other PR".into();
        let bytes = serde_json::to_vec(&drafts).unwrap();
        let loaded: HashMap<String, Draft> = serde_json::from_slice(&bytes).unwrap();
        let first = &loaded["https://github.com/o/r/pull/1"];
        assert_eq!(
            first.description.as_ref().unwrap().original_body,
            "baseline"
        );
        assert_eq!(first.replies["thread-b"], "second");
        assert_eq!(loaded["https://github.com/o/r/pull/2"].note, "other PR");
    }
}

#[cfg(test)]
pub(crate) struct TestState<'a> {
    pub selected: usize,
    pub details: &'a Option<api::Details>,
    pub loading: bool,
    pub error: &'a Option<String>,
    pub save_error: &'a Option<String>,
    pub archiving: bool,
}
