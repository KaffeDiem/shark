//! The Git artifact: GitHub conversations, with drafts pinned to their exact PR.
pub(crate) mod commits;
pub(crate) mod document;
mod drafts;
mod mermaid;
use crate::media::{Lru, Playing};
use crate::pr::git_feature as api;
use crate::state::Workspace;
use crate::ui::confirm;
use crate::{
    CancelEdit, PageDown, PageUp, SaveFile, SelectFirst, SelectLast, SelectNext, SelectPrev,
};
use commits::{Commits, RepoContext};
use document::{DocContext, RichDoc};
use drafts::{DescriptionDraft, DraftStore, Editing};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, Textarea, TextareaState};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::component::{Disableable as _, Selectable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use std::{collections::HashMap, path::PathBuf};

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

/// A message open in the reader; `source` is its read-only Markdown source, when switched to.
struct Reader {
    doc: Entity<RichDoc>,
    source: Option<Entity<EditorState>>,
}

/// What the Git pane shows; both keep their state while the other is shown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum GitView {
    Conversation,
    Commits,
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
    archiving: bool,
    posting: bool,
    generation: u64,
    refreshed: Option<Instant>,
    focus: FocusHandle,
    composer: Option<Composer>,
    reader: Option<Reader>,
    store: DraftStore,
    rows: Vec<Row>,
    markdown: HashMap<String, Entity<RichDoc>>,
    markdown_order: VecDeque<String>,
    media_cache: Entity<Lru>,
    view: GitView,
    commits: Entity<Commits>,
    list: ListState,
    show_resolved: bool,
    show_drafts: bool,
}

impl GitFeature {
    pub(crate) fn new(workspace: Workspace, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = DraftStore::load(&workspace.name);
        let media_cache = Lru::new(cx);
        let commits = cx.new(|cx| Commits::new(media_cache.clone(), cx));
        commits.update(cx, |c, cx| c.set_repo(repo_context(&workspace, 0), cx));
        Self {
            workspace,
            selected: 0,
            details: None,
            loading: false,
            loaded: false,
            error: None,
            archiving: false,
            posting: false,
            generation: 0,
            refreshed: None,
            focus: cx.focus_handle(),
            composer: None,
            reader: None,
            store,
            rows: Vec::new(),
            markdown: HashMap::new(),
            markdown_order: VecDeque::new(),
            media_cache,
            view: GitView::Conversation,
            commits,
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            show_resolved: false,
            show_drafts: false,
        }
    }

    /// A base change (the host passes its effective bases) resets only the history.
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
        self.sync_commits(cx);
    }

    fn sync_commits(&mut self, cx: &mut Context<Self>) {
        let repo = repo_context(&self.workspace, self.selected);
        let visible = self.view == GitView::Commits;
        self.commits.update(cx, |c, cx| {
            c.set_repo(repo, cx);
            if visible {
                c.ensure_loaded(cx);
            }
        });
    }

    pub(crate) fn set_view(&mut self, view: GitView, window: &mut Window, cx: &mut Context<Self>) {
        if self.view == view {
            return;
        }
        self.stop_media(cx);
        self.view = view;
        match view {
            GitView::Commits => {
                self.commits.update(cx, |c, cx| c.ensure_loaded(cx));
                self.commits
                    .read(cx)
                    .focus_handle()
                    .clone()
                    .focus(window, cx);
            }
            GitView::Conversation => self.focus.focus(window, cx),
        }
        cx.notify();
    }

    /// Leaving a view or message stops its videos.
    fn stop_media(&mut self, cx: &mut Context<Self>) {
        for doc in self
            .markdown
            .values()
            .chain(self.reader.as_ref().map(|r| &r.doc))
        {
            doc.update(cx, |d, cx| d.stop(cx));
        }
        self.commits.update(cx, |c, cx| c.stop(cx));
    }

    #[cfg(test)]
    pub(crate) fn commits(&self) -> &Entity<Commits> {
        &self.commits
    }

    pub(crate) fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.view == GitView::Commits {
            self.commits.update(cx, |c, cx| c.ensure_loaded(cx));
            self.commits
                .read(cx)
                .focus_handle()
                .clone()
                .focus(window, cx);
        } else {
            self.focus.focus(window, cx);
        }
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
        self.store.error.as_deref()
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
            save_error: &self.store.error,
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
        self.sync_commits(cx);
        self.refresh(window, cx);
    }

    fn wt(&self) -> Option<PathBuf> {
        let repo = self.workspace.repos.get(self.selected)?;
        Some(self.workspace.dir().join(&repo.name))
    }

    pub(crate) fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading || self.posting || self.archiving {
            return;
        }
        let Some(wt) = self.wt() else {
            return;
        };
        let branch = self
            .workspace
            .branch_of(&self.workspace.repos[self.selected])
            .to_string();
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
        self.reader = None;
        self.rows.clear();
        // Dropping a document drops its players, which pauses them.
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
        self.store
            .drafts
            .get(target)
            .is_some_and(|draft| draft.has(kind))
    }

    fn local_drafts(&self) -> Vec<(String, Editing)> {
        let mut targets: Vec<_> = self.store.drafts.keys().collect();
        targets.sort();
        let mut entries = Vec::new();
        for target in targets {
            let draft = &self.store.drafts[target];
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
        self.reader = None;
        self.capture_draft(cx);
        let details = self.details.as_ref().filter(|d| d.target.url() == target);
        if !self.has_draft(&target, &kind) && details.is_none() {
            return;
        }
        let draft = self.store.drafts.entry(target.clone()).or_default();
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
            self.store.dirty(cx);
        }
        cx.notify();
    }

    fn capture_draft(&mut self, cx: &mut Context<Self>) {
        let Some(c) = &self.composer else {
            return;
        };
        let draft = self.store.drafts.entry(c.target.clone()).or_default();
        let before = draft.clone();
        let (title, body) = (
            c.title.read(cx).value().to_string(),
            c.body.read(cx).value().to_string(),
        );
        draft.set(&c.kind, title, body);
        if *draft != before {
            self.store.dirty(cx);
            cx.notify();
        }
    }

    pub(crate) fn saving_drafts(&self) -> bool {
        self.store.saving()
    }

    pub(crate) fn retry_save(&mut self, cx: &mut Context<Self>) {
        self.capture_draft(cx);
        self.store.schedule_save(cx);
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
            if let Some(d) = self.store.drafts.get_mut(&c.target) {
                d.clear(&c.kind);
            }
            self.store.dirty(cx);
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
                .store
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
                    .store
                    .drafts
                    .get_mut(&target)
                    .and_then(|draft| draft.description.as_mut())
                {
                    draft.original_title = title.clone();
                    draft.original_body = body.clone();
                    s.store.dirty(cx);
                }
                s.submit(w, cx);
            },
            window,
            cx,
        );
    }

    pub(crate) fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A composer hidden behind the reader or the Commits view never posts.
        if self.reader.is_some() || self.view != GitView::Conversation || !self.can_submit() {
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
        let sent_kind = kind.clone();
        let title = c.title.read(cx).value().to_string();
        let body = c.body.read(cx).value().to_string();
        if matches!(kind, Editing::Description) {
            if title.trim().is_empty() {
                self.error = Some("A PR title cannot be empty".into());
                cx.notify();
                return;
            }
            if let Some(d) = self
                .store
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
        let Some(wt) = self.wt() else {
            return;
        };
        let target = c.target.clone();
        self.post(
            move || match &kind {
                Editing::Description => api::save_description(&wt, &details, &title, &body),
                Editing::Comment => api::comment(&wt, &details, &body),
                Editing::Reply(id) => api::reply(&wt, &details, id, &body),
            },
            move |s, cx| {
                if let Some(d) = s.store.drafts.get_mut(&target) {
                    d.clear(&sent_kind);
                }
                if s.composer.as_ref().is_some_and(|c| c.target == target) {
                    s.composer = None;
                }
                s.store.dirty(cx);
            },
            window,
            cx,
        );
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
        let Some(wt) = self.wt() else {
            return;
        };
        let id = id.to_string();
        self.post(
            move || api::resolve(&wt, &d, &id, resolved),
            |_, _| {},
            window,
            cx,
        );
    }

    /// Runs a GitHub write off-thread, then applies `on_ok` and refreshes; failures land in `error`.
    fn post(
        &mut self,
        work: impl FnOnce() -> Result<(), String> + Send + 'static,
        on_ok: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.posting = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move { work() }).await;
            this.update_in(cx, |s, window, cx| {
                s.posting = false;
                match result {
                    Ok(()) => {
                        on_ok(s, cx);
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

    /// Relative links in PR text resolve against the PR head the conversation was read at.
    fn doc_context(&self) -> Option<DocContext> {
        let d = self.details.as_ref()?;
        let repo = self.workspace.repos.get(self.selected)?;
        Some(DocContext {
            repo: self.workspace.dir().join(&repo.name),
            rev: d.head_sha.clone(),
            dir: String::new(),
            web: Some(format!(
                "https://{}/{}/{}",
                d.target.host, d.target.owner, d.target.repo
            )),
        })
    }

    fn rich(&mut self, source: String, full: bool, cx: &mut Context<Self>) -> Entity<RichDoc> {
        let (ctx, cache) = (self.doc_context(), self.media_cache.clone());
        let doc = cx.new(|_| RichDoc::new(source, ctx, cache, full));
        // One video plays at a time across the conversation. Detached: it ends with the doc.
        cx.subscribe(&doc, |s, playing, _: &Playing, cx| {
            for doc in s.markdown.values().chain(s.reader.as_ref().map(|r| &r.doc)) {
                if *doc != playing {
                    doc.update(cx, |d, cx| d.stop(cx));
                }
            }
        })
        .detach();
        doc
    }

    fn md(&mut self, id: String, body: &str, cx: &mut Context<Self>) -> Entity<RichDoc> {
        if let Some(at) = self.markdown_order.iter().position(|key| key == &id) {
            self.markdown_order.remove(at);
        }
        self.markdown_order.push_back(id.clone());
        if self.markdown_order.len() > 128
            && let Some(oldest) = self.markdown_order.pop_front()
        {
            self.markdown.remove(&oldest);
        }
        if let Some(doc) = self.markdown.get(&id) {
            return doc.clone();
        }
        let doc = self.rich(markdown_preview(body).to_string(), false, cx);
        self.markdown.insert(id, doc.clone());
        doc
    }

    fn open_reader(&mut self, body: String, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_media(cx);
        let doc = self.rich(body, true, cx);
        self.reader = Some(Reader { doc, source: None });
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn show_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(body) = (self.reader.as_ref()).map(|r| r.doc.read(cx).source().to_string()) else {
            return;
        };
        self.stop_media(cx);
        let input = cx.new(|cx| {
            EditorState::new(window, cx)
                .default_value(body)
                .soft_wrap(true)
        });
        input.update(cx, |i, cx| i.set_readonly(true, cx));
        input.read(cx).focus_handle(cx).focus(window, cx);
        if let Some(r) = &mut self.reader {
            r.source = Some(input);
        }
        cx.notify();
    }

    fn close_reader(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_media(cx);
        self.reader = None;
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn body_view(&mut self, id: String, body: String, cx: &mut Context<Self>) -> AnyElement {
        let md = self.md(id.clone(), &body, cx);
        // Every message opens in the reader, which also offers its Markdown source.
        let long = body.len() > PREVIEW_BYTES;
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .text_xs()
                    .when(long, |d| d.child("Preview of a long message"))
                    .child(
                        Button::new(SharedString::from(format!("git-long-{id}")))
                            .label(if long { "Read full text" } else { "Reader" })
                            .map(|b| match long {
                                true => b.small(),
                                false => b.ghost().xsmall().tooltip("Read with source"),
                            })
                            .on_click(
                                cx.listener(move |s, _, w, cx| s.open_reader(body.clone(), w, cx)),
                            ),
                    ),
            )
            .child(md)
            .into_any_element()
    }

    fn overview(&self, cx: &mut Context<Self>) -> AnyElement {
        let d = self.details.as_ref().unwrap();
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let disabled = self.posting || self.archiving;
        let url = d.url.clone();
        let draft = self.store.drafts.get(&d.target.url());
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
                    .store
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
        match &self.reader {
            Some(r) => r.doc.read(cx).scroll_by(px(lines * 21.)),
            None => self.list.scroll_by(px(lines * 21.)),
        }
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn delay_next_save(&mut self, delay: Duration) {
        self.store.delay = delay;
    }

    #[cfg(test)]
    pub(crate) fn save_started(&self) -> u64 {
        self.store.started
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
        (self.reader.as_ref())
            .and_then(|r| r.source.as_ref())
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
            .map(|m| m.read(cx).rendered_len(cx))
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub(crate) fn markdown_doc(&self, key: &str) -> Option<&Entity<RichDoc>> {
        self.markdown.get(key)
    }

    #[cfg(test)]
    pub(crate) fn reader(&self) -> Option<&Entity<RichDoc>> {
        self.reader.as_ref().map(|r| &r.doc)
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

/// The selected repository's history source: its worktree and effective comparison base.
fn repo_context(workspace: &Workspace, selected: usize) -> Option<RepoContext> {
    let repo = workspace.repos.get(selected)?;
    Some(RepoContext {
        wt: workspace.dir().join(&repo.name),
        base: repo.base.clone(),
    })
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

impl GitFeature {
    fn toolbar(&self, drafts: usize, cx: &mut Context<Self>) -> AnyElement {
        let border = cx.theme().border;
        let disabled = self.posting || self.archiving;
        let repo = self.workspace.repos.get(self.selected);
        let commits_view = self.view == GitView::Commits;
        h_flex()
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
            .child(div().w(px(8.)))
            .child(
                Button::new("git-view-conversation")
                    .label("Conversation")
                    .icon(IconName::MessageSquare)
                    .small()
                    .ghost()
                    .selected(!commits_view)
                    .on_click(cx.listener(|s, _, w, cx| s.set_view(GitView::Conversation, w, cx))),
            )
            .child(
                Button::new("git-view-commits")
                    .label("Commits")
                    .icon(IconName::GitBranch)
                    .small()
                    .ghost()
                    .selected(commits_view)
                    .on_click(cx.listener(|s, _, w, cx| s.set_view(GitView::Commits, w, cx))),
            )
            .child(div().flex_1())
            .when(!commits_view, |r| {
                r.when(drafts > 0 || self.show_drafts, |r| {
                    r.child(
                        Button::new("git-local-drafts")
                            .label(if self.show_drafts {
                                "Conversation".to_string()
                            } else {
                                format!("Local drafts ({})", drafts)
                            })
                            .small()
                            .ghost()
                            .disabled(disabled)
                            .on_click(cx.listener(|s, _, w, cx| {
                                s.show_drafts = !s.show_drafts;
                                s.reader = None;
                                s.focus.focus(w, cx);
                                cx.notify();
                            })),
                    )
                })
                .when_some(self.details.as_ref(), |r, d| {
                    let resume_comment = self
                        .store
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
                )
            })
            .into_any_element()
    }

    /// Icon, title and detail shown while there is no pull request to list.
    fn placeholder_state(&self) -> (&'static str, &'static str, &'static str, IconName) {
        if self.workspace.repos.get(self.selected).is_none() {
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
        }
    }

    fn placeholder(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let disabled = self.posting || self.archiving;
        let repo = self.workspace.repos.get(self.selected);
        let (state_id, state_title, state_detail, state_icon) = self.placeholder_state();
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
    }

    fn drafts_list(&self, drafts: Vec<(String, Editing)>, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let disabled = self.posting || self.archiving;
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
                            self.store
                                .drafts
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
    }

    fn reader_view(&self, reader: &Reader, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let (doc, source) = (reader.doc.clone(), reader.source.clone());
        v_flex()
            .size_full()
            .min_h_0()
            .child(
                h_flex()
                    .px_4()
                    .py_2()
                    .gap_2()
                    .flex_none()
                    .flex_wrap()
                    .child(
                        Button::new("git-full-close")
                            .icon(IconName::ArrowLeft)
                            .label("Back to conversation")
                            .small()
                            .on_click(cx.listener(|s, _, w, cx| s.close_reader(w, cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("git-full-preview")
                            .label("Preview")
                            .small()
                            .ghost()
                            .selected(source.is_none())
                            .on_click(cx.listener(|s, _, w, cx| {
                                if let Some(r) = &mut s.reader {
                                    r.source = None;
                                }
                                s.focus.focus(w, cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("git-full-source")
                            .label("Source")
                            .small()
                            .ghost()
                            .selected(source.is_some())
                            .on_click(cx.listener(|s, _, w, cx| s.show_source(w, cx))),
                    ),
            )
            .when_some(source.as_ref(), |d, _| {
                d.child(
                    div()
                        .px_4()
                        .pb_2()
                        .text_xs()
                        .text_color(muted)
                        .child("Full Markdown text · read only · ⌘F to search"),
                )
            })
            .child(match source {
                Some(input) => div()
                    .id("git-full-text")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .child(
                        Editor::new(&input)
                            .readonly(true)
                            .appearance(false)
                            .h_full(),
                    )
                    .into_any_element(),
                None => div()
                    .id("git-full-reader")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .child(doc)
                    .into_any_element(),
            })
            .into_any_element()
    }

    fn composer_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme();
        let (border, muted) = (theme.border, theme.muted_foreground);
        let disabled = self.posting || self.archiving;
        let commits_view = self.view == GitView::Commits;
        self.composer
            .as_ref()
            .filter(|_| self.reader.is_none() && !commits_view)
            .map(|c| {
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
                .into_any_element()
        })
    }
}

fn banner(warning: Hsla, body: impl IntoElement) -> Div {
    h_flex()
        .px_4()
        .py_2()
        .gap_2()
        .items_start()
        .text_sm()
        .child(
            Icon::new(IconName::TriangleAlert)
                .with_size(px(16.))
                .text_color(warning)
                .flex_none(),
        )
        .child(body)
}

impl Render for GitFeature {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (warning, muted) = (theme.warning_foreground, theme.muted_foreground);
        let drafts = self.local_drafts();
        let toolbar = self.toolbar(drafts.len(), cx);
        let commits_view = self.view == GitView::Commits;
        let content = if commits_view {
            self.commits.clone().into_any_element()
        } else if self.show_drafts {
            self.drafts_list(drafts, cx)
        } else if let Some(reader) = &self.reader {
            self.reader_view(reader, cx)
        } else if self.details.is_some() {
            list(
                self.list.clone(),
                cx.processor(|s, i, w, cx| s.render_row(i, w, cx)),
            )
            .size_full()
            .py_2()
            .into_any_element()
        } else {
            self.placeholder(cx)
        };
        let composer = self.composer_view(cx);
        v_flex()
            .id("git-feature")
            .key_context("GitFeature")
            .on_action(cx.listener(|s, _: &SaveFile, w, cx| s.submit(w, cx)))
            .on_action(cx.listener(|s, _: &SelectNext, _, cx| s.scroll(2., cx)))
            .on_action(cx.listener(|s, _: &SelectPrev, _, cx| s.scroll(-2., cx)))
            .on_action(cx.listener(|s, _: &PageDown, _, cx| s.scroll(15., cx)))
            .on_action(cx.listener(|s, _: &PageUp, _, cx| s.scroll(-15., cx)))
            .on_action(cx.listener(|s, _: &SelectFirst, _, cx| {
                match &s.reader {
                    Some(r) => r.doc.read(cx).scroll_to_start(),
                    None => s.list.scroll_to(ListOffset::default()),
                }
                cx.notify();
            }))
            .on_action(cx.listener(|s, _: &SelectLast, _, cx| {
                match &s.reader {
                    Some(r) => r.doc.read(cx).scroll_to_end(),
                    None => s.list.scroll_to_end(),
                }
                cx.notify();
            }))
            .on_action(cx.listener(|s, _: &CancelEdit, w, cx| {
                if !s.posting {
                    if s.reader.is_some() {
                        s.close_reader(w, cx);
                    } else {
                        s.close_editor(cx);
                    }
                    // Escape at the commit list keeps focus there, so arrows still move it.
                    match s.view {
                        GitView::Commits => s.commits.read(cx).focus_handle().clone().focus(w, cx),
                        GitView::Conversation => s.focus.focus(w, cx),
                    }
                }
            }))
            .track_focus(&self.focus)
            .aria_label("Git pull request")
            .size_full()
            .min_h_0()
            .min_w_0()
            .child(toolbar)
            .when_some(self.store.error.clone(), |d, e| {
                d.child(banner(
                    warning,
                    div().min_w_0().child(format!("Draft not saved: {e}")),
                ))
            })
            .when_some(
                self.error
                    .clone()
                    .filter(|_| self.details.is_some() && !commits_view),
                |d, e| {
                    d.child(
                        banner(
                            warning,
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
                        )
                        .id("git-error")
                        .test_support(),
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
pub(crate) struct TestState<'a> {
    pub selected: usize,
    pub details: &'a Option<api::Details>,
    pub loading: bool,
    pub error: &'a Option<String>,
    pub save_error: &'a Option<String>,
    pub archiving: bool,
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
}
