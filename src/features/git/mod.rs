//! The Git artifact: GitHub and Azure DevOps conversations, with drafts pinned to their exact
//! PR, and on Azure DevOps the work items (PBIs and their tasks) the branch is for.
pub(crate) mod commits;
pub(crate) mod document;
mod drafts;
use crate::media::{Lru, Playing};
use crate::pr::git_feature as api;
use crate::state::Task;
use crate::ui::{confirm, faint};
use crate::{
    CancelEdit, CommentOnPr, Confirm, EditDescription, PageDown, PageUp, ReplyToThread, SaveFile,
    SelectFirst, SelectLast, SelectNext, SelectPrev,
};
use commits::{Commits, RepoContext};
use document::{DocContext, RichDoc};
pub(crate) use drafts::store_path as drafts_path;
use drafts::{DescriptionDraft, DraftStore, Editing};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::InputEvent;
use gpui_kit::component::input::{Editor, EditorState, Enter, Textarea, TextareaState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::component::{Disableable as _, Selectable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};
use std::{collections::HashMap, path::PathBuf};

struct Composer {
    target: String,
    kind: Editing,
    title: Entity<TextareaState>,
    body: Entity<TextareaState>,
    _subscriptions: Vec<Subscription>,
}

/// One cursor stop in the conversation: the PR header, a timeline item, or the comment composer.
#[derive(Clone, Debug, PartialEq)]
enum Row {
    Header,
    Comment(usize),
    Review(usize),
    /// A review thread, shown after its review when it has one.
    Thread(usize),
    Empty,
    Composer,
}

/// A message open in the reader; `source` is its read-only Markdown source, when switched to.
struct Reader {
    doc: Entity<RichDoc>,
    source: Option<Entity<EditorState>>,
    /// The message shown: `description` or a comment id.
    key: String,
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
    task: Task,
    selected: usize,
    details: Option<api::Details>,
    /// Azure DevOps only; a failure here doesn't hide the PR.
    work_items: Result<Vec<crate::azure::WorkItem>, String>,
    loading: bool,
    loaded: bool,
    error: Option<String>,
    archiving: bool,
    posting: bool,
    /// The editor whose text is being posted; only that editor is disabled meanwhile.
    sending: Option<Editing>,
    generation: u64,
    refreshed: Option<Instant>,
    focus: FocusHandle,
    composer: Option<Composer>,
    reader: Option<Reader>,
    store: DraftStore,
    rows: Vec<Row>,
    /// `row_key` of each row, so the cursor and scroll follow items across reloads.
    row_ids: Vec<String>,
    markdown: HashMap<String, Entity<RichDoc>>,
    markdown_order: VecDeque<String>,
    /// Markdown keys whose preview was cut; they get "Show more".
    cut: HashSet<String>,
    media_cache: Entity<Lru>,
    view: GitView,
    commits: Entity<Commits>,
    list: ListState,
    /// The keyboard cursor, an index into `rows`.
    cursor: usize,
    /// The cursor is drawn after keyboard navigation, not after a click.
    kbd_cursor: bool,
    /// An editor just opened in this item: reveal it (top first?) once it has been measured.
    reveal_pending: Option<(usize, bool, bool)>,
    /// Resolved threads and hidden comments the user opened, by id.
    expanded: HashSet<String>,
    show_drafts: bool,
}

impl GitFeature {
    pub(crate) fn new(task: Task, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = DraftStore::load(&task.name);
        let media_cache = Lru::new(cx);
        let commits = cx.new(|cx| Commits::new(media_cache.clone(), cx));
        commits.update(cx, |c, cx| c.set_repo(repo_context(&task, 0), cx));
        Self {
            task,
            selected: 0,
            details: None,
            work_items: Ok(vec![]),
            loading: false,
            loaded: false,
            error: None,
            archiving: false,
            posting: false,
            sending: None,
            generation: 0,
            refreshed: None,
            focus: cx.focus_handle(),
            composer: None,
            reader: None,
            store,
            rows: Vec::new(),
            row_ids: Vec::new(),
            markdown: HashMap::new(),
            markdown_order: VecDeque::new(),
            cut: HashSet::new(),
            media_cache,
            view: GitView::Conversation,
            commits,
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            cursor: 0,
            kbd_cursor: false,
            reveal_pending: None,
            expanded: HashSet::new(),
            show_drafts: false,
        }
    }

    /// A base change (the host passes its effective bases) resets only the history.
    pub(crate) fn set_task(&mut self, task: Task, window: &mut Window, cx: &mut Context<Self>) {
        let identity = |ws: &Task| {
            ws.repos
                .iter()
                .map(|r| (r.name.clone(), r.src.clone(), ws.branch_of(r).to_string()))
                .collect::<Vec<_>>()
        };
        let changed = identity(&self.task) != identity(&task);
        self.task = task;
        if changed {
            self.selected = self.selected.min(self.task.repos.len().saturating_sub(1));
            self.clear_selection(cx);
            self.refresh(window, cx);
        }
        self.sync_commits(cx);
    }

    fn sync_commits(&mut self, cx: &mut Context<Self>) {
        let repo = repo_context(&self.task, self.selected);
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
            self.commits.read(cx).focus_target(cx).focus(window, cx);
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
            cursor: self.cursor,
            rows: self.row_ids.clone(),
            editing: self.composer.as_ref().map(|c| format!("{:?}", c.kind)),
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
        self.work_items = Ok(vec![]);
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
        if self.posting || index >= self.task.repos.len() || self.selected == index {
            return;
        }
        self.clear_selection(cx);
        self.selected = index;
        self.sync_commits(cx);
        self.refresh(window, cx);
    }

    fn wt(&self) -> Option<PathBuf> {
        let repo = self.task.repos.get(self.selected)?;
        Some(self.task.dir().join(&repo.name))
    }

    pub(crate) fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading || self.posting || self.archiving {
            return;
        }
        let Some(wt) = self.wt() else {
            return;
        };
        let branch = self
            .task
            .branch_of(&self.task.repos[self.selected])
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
                    let details = api::discover(&wt, &branch)?
                        .map(|target| api::load(&wt, &target))
                        .transpose()?;
                    let pr = (details.as_ref())
                        .filter(|d| d.target.remote().is_some())
                        .map(|d| d.target.number);
                    Ok::<_, String>((details, crate::azure::work_items(&wt, &branch, pr)))
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
                    Ok((details, work_items)) => {
                        s.work_items = work_items;
                        let target_changed = s.details.as_ref().map(|d| &d.target)
                            != details.as_ref().map(|d| &d.target);
                        if target_changed {
                            s.rows.clear();
                            s.row_ids.clear();
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
        let old = std::mem::take(&mut self.row_ids);
        let (at, top) = (
            old.get(self.cursor).cloned(),
            old.get(offset.item_ix).cloned(),
        );
        // A refresh keeps the reader while its message still exists.
        if (self.reader.as_ref()).is_some_and(|r| {
            !self
                .details
                .as_ref()
                .is_some_and(|d| has_message(d, &r.key))
        }) {
            self.reader = None;
        }
        // Dropping a document drops its players, which pauses them.
        self.markdown.clear();
        self.markdown_order.clear();
        self.cut.clear();
        self.rows = match &self.details {
            Some(d) => {
                let composer = d.can_comment || self.has_draft(&d.target.url(), &Editing::Comment);
                timeline(d, composer)
            }
            None => Vec::new(),
        };
        if let Some(d) = &self.details {
            self.row_ids = self.rows.iter().map(|r| row_key(d, r)).collect();
        }
        let find =
            |id: Option<String>| id.and_then(|id| self.row_ids.iter().position(|r| *r == id));
        let last = self.rows.len().saturating_sub(1);
        // The cursor follows its item; another PR starts at the top.
        self.cursor = match find(at) {
            Some(ix) => ix,
            None if old.is_empty() => 0,
            None => self.cursor.min(last),
        };
        self.list.reset(self.rows.len());
        if !old.is_empty() && !self.rows.is_empty() {
            self.list.scroll_to(match find(top) {
                Some(item_ix) => ListOffset { item_ix, ..offset },
                None => ListOffset {
                    item_ix: offset.item_ix.min(last),
                    ..offset
                },
            });
        }
    }

    /// Moves the cursor and scrolls it into view; an item taller than the pane shows its top.
    fn set_cursor(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        self.cursor = ix.min(self.rows.len() - 1);
        self.reveal(self.cursor, true);
        cx.notify();
    }

    /// Scrolls item `ix` into view. With `top`, an item taller than the pane shows its top,
    /// otherwise its bottom (where a reply or comment editor sits).
    fn reveal(&self, ix: usize, top: bool) {
        // Far items have no measured height yet; bring them in first, refine next frame.
        if self.list.bounds_for_item(ix).is_none()
            && self.list.item_is_above_viewport(ix) != Some(true)
        {
            match ix + 1 == self.rows.len() {
                true => self.list.scroll_to_end(),
                false => self.list.scroll_to(ListOffset {
                    item_ix: ix,
                    offset_in_item: px(0.),
                }),
            }
            return;
        }
        self.list.scroll_to_reveal_item(ix);
        let at = self.list.logical_scroll_top();
        if top && at.item_ix == ix && at.offset_in_item > px(0.) {
            self.list.scroll_to(ListOffset {
                item_ix: ix,
                offset_in_item: px(0.),
            });
        }
    }

    fn move_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        // After scrolling away with the wheel or Page keys, start from what is on screen.
        let away = self.list.item_is_above_viewport(self.cursor) != Some(false)
            || self.list.item_is_below_viewport(self.cursor) != Some(false);
        let from = if away {
            self.list.logical_scroll_top().item_ix as isize
        } else {
            self.cursor as isize + delta
        };
        self.set_cursor(from.max(0) as usize, cx);
    }

    /// Enter on the cursor: edit the description, reply to or expand a thread, expand a hidden
    /// comment, read a comment in full, or write a comment.
    fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(row), Some(d)) = (self.rows.get(self.cursor).cloned(), &self.details) else {
            return;
        };
        match row {
            Row::Header => self.edit_description(false, window, cx),
            Row::Comment(i) | Row::Review(i) => {
                let c = match row {
                    Row::Comment(_) => &d.comments[i],
                    _ => &d.reviews[i].comment,
                };
                if c.minimized && !self.expanded.contains(&c.id) {
                    self.expanded.insert(c.id.clone());
                    cx.notify();
                } else if !c.body.trim().is_empty() {
                    let (key, body) = (c.id.clone(), c.body.clone());
                    self.open_reader(key, body, window, cx);
                }
            }
            Row::Thread(i) => {
                let t = &d.threads[i];
                if t.resolved && !self.expanded.contains(&t.id) {
                    self.expanded.insert(t.id.clone());
                    cx.notify();
                } else {
                    let id = t.id.clone();
                    self.start_reply(&id, window, cx);
                }
            }
            Row::Composer => self.start_comment(window, cx),
            Row::Empty => {}
        }
    }

    /// `r`: reply to the cursor's thread.
    fn reply_at_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let (Some(Row::Thread(i)), Some(d)) = (self.rows.get(self.cursor), &self.details) {
            let id = d.threads[*i].id.clone();
            self.expanded.insert(id.clone());
            self.start_reply(&id, window, cx);
        }
    }

    fn toggle_expanded(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.expanded.remove(id) {
            self.expanded.insert(id.to_string());
        } else if (self.composer.as_ref()).is_some_and(|c| c.kind == Editing::Reply(id.into())) {
            // Collapsing a thread closes its reply editor; the draft stays.
            self.close_editor(cx);
        }
        cx.notify();
    }

    /// With an editor open but its field not focused (clicked away, or scrolled out of the
    /// list), e/r/c/Enter return to that editor instead of starting or changing another.
    fn return_to_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(c) = &self.composer else {
            return false;
        };
        let (kind, body) = (c.kind.clone(), c.body.clone());
        let row = match &kind {
            Editing::Description => Some(Row::Header),
            Editing::Comment => Some(Row::Composer),
            Editing::Reply(id) => (self.details.as_ref())
                .and_then(|d| d.threads.iter().position(|t| t.id == *id))
                .map(Row::Thread),
        };
        if let Some(ix) = row
            .filter(|_| self.placed())
            .and_then(|row| self.rows.iter().position(|r| *r == row))
        {
            self.cursor = ix;
            self.reveal(ix, kind == Editing::Description);
            self.reveal_pending = Some((ix, kind == Editing::Description, false));
        }
        body.update(cx, |b, cx| b.focus(window, cx));
        cx.notify();
        true
    }

    pub(crate) fn start_description(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_description(false, window, cx);
    }

    /// Opens the title and description in place, focused on the title or the body.
    fn edit_description(&mut self, title: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .details
            .as_ref()
            .is_some_and(|d| d.can_update || self.has_draft(&d.target.url(), &Editing::Description))
        {
            self.start_editor(Editing::Description, title, window, cx);
        }
    }

    pub(crate) fn start_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .details
            .as_ref()
            .is_some_and(|d| d.can_comment || self.has_draft(&d.target.url(), &Editing::Comment))
        {
            self.start_editor(Editing::Comment, false, window, cx);
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
            self.start_editor(Editing::Reply(thread.to_string()), false, window, cx);
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

    fn start_editor(
        &mut self,
        kind: Editing,
        title: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(d) = &self.details else {
            return;
        };
        let target = d.target.url();
        // The editor opens where its text belongs; the cursor follows it there.
        let row = match &kind {
            Editing::Description => Some(Row::Header),
            Editing::Comment => Some(Row::Composer),
            Editing::Reply(id) => (d.threads.iter())
                .position(|t| t.id == *id)
                .map(Row::Thread),
        };
        if let Editing::Reply(id) = &kind {
            self.expanded.insert(id.clone());
        }
        let top = kind == Editing::Description;
        self.show_drafts = false;
        self.open_draft(target, kind, title, window, cx);
        if let Some(ix) = row.and_then(|row| self.rows.iter().position(|r| *r == row)) {
            self.cursor = ix;
            self.reveal(ix, top);
            // Again once a frame has measured the opened editor.
            self.reveal_pending = Some((ix, top, false));
            cx.notify();
        }
    }

    fn open_draft(
        &mut self,
        target: String,
        kind: Editing,
        focus_title: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Another editor may open while one posts; the one being sent stays closed until done.
        if self.archiving || self.sending.as_ref() == Some(&kind) {
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
                // An unchanged description isn't a draft: start again from GitHub's copy.
                if !draft.description.as_ref().is_some_and(|d| d.changed())
                    && let Some(d) = details
                {
                    draft.description = Some(DescriptionDraft {
                        original_title: d.title.clone(),
                        original_body: d.body.clone(),
                        title: d.title.clone(),
                        body: d.body.clone(),
                    });
                }
                let Some(edit) = draft.description.as_ref() else {
                    return;
                };
                (edit.title.clone(), edit.body.clone())
            }
            Editing::Comment => (String::new(), draft.note.clone()),
            Editing::Reply(id) => (
                String::new(),
                draft.replies.get(id).cloned().unwrap_or_default(),
            ),
        };
        let draft_changed = *draft != before;
        // A title wraps like the one it replaces.
        let title = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 4)
                .default_value(title)
                .placeholder("PR title")
        });
        let (rows, placeholder) = match &kind {
            Editing::Description => ((4, 30), "Add description…"),
            Editing::Comment => ((3, 16), "Leave a comment…"),
            Editing::Reply(_) => ((2, 16), "Leave a reply…"),
        };
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(rows.0, rows.1)
                .default_value(body)
                .placeholder(placeholder)
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
        if focus_title {
            title.update(cx, |t, cx| t.focus(window, cx));
        } else {
            body.update(cx, |b, cx| {
                let end = b.value().len();
                b.set_selected_range(end..end, cx);
                b.focus(window, cx);
            });
        }
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
        if self.composer.as_ref().map(|c| &c.kind) == self.sending.as_ref() {
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
        // A pasted line break cannot make a title span lines on GitHub.
        let title = c.title.read(cx).value().replace(['\r', '\n'], " ");
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
        self.sending = Some(kind.clone());
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
                if s.composer
                    .as_ref()
                    .is_some_and(|c| c.target == target && c.kind == sent_kind)
                {
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
                s.sending = None;
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
        let repo = self.task.repos.get(self.selected)?;
        Some(DocContext {
            repo: self.task.dir().join(&repo.name),
            rev: d.head_sha.clone(),
            dir: String::new(),
            web: Some(format!(
                "https://{}/{}/{}",
                d.target.host, d.target.owner, d.target.repo
            )),
            on_disk: false,
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
        // Rich text lays out whole paragraphs; previews stay bounded until it virtualizes them.
        let (text, cut) = document::preview(body, PREVIEW_BYTES);
        if cut {
            self.cut.insert(id.clone());
        }
        let doc = self.rich(text, false, cx);
        self.markdown.insert(id, doc.clone());
        doc
    }

    fn open_reader(
        &mut self,
        key: String,
        body: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.stop_media(cx);
        let doc = self.rich(body, true, cx);
        self.reader = Some(Reader {
            doc,
            source: None,
            key,
        });
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
        let cut = self.cut.contains(&id);
        v_flex()
            .w_full()
            .min_w_0()
            .child(md)
            .when(cut, |d| {
                d.child(
                    h_flex().child(
                        Button::new(SharedString::from(format!("git-long-{id}")))
                            .label("Show more")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |s, _, w, cx| {
                                // Inside the description, which edits on click.
                                cx.stop_propagation();
                                s.open_reader(id.clone(), body.clone(), w, cx)
                            })),
                    ),
                )
            })
            .into_any_element()
    }

    /// The editor's text area plus its notes and buttons; the title field sits in the header.
    /// `boxed`: the text sits in an `edit_box` (description, reply); otherwise the card around
    /// it draws the frame (comment composer, pinned editor).
    fn editor_body(&self, boxed: bool, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(c) = &self.composer else {
            return div().into_any_element();
        };
        let focused = c.body.read(cx).focus_handle(cx).is_focused(window);
        let theme = cx.theme();
        let (muted, faint) = (theme.muted_foreground, faint(theme));
        let sending = self.sending.as_ref() == Some(&c.kind);
        let locked = self.archiving || sending;
        let label = match c.kind {
            Editing::Description => "Save",
            Editing::Comment => "Comment",
            Editing::Reply(_) => "Reply",
        };
        v_flex()
            .gap_2()
            .min_w_0()
            .child(
                div()
                    .id("git-editor-body")
                    .test_support()
                    .when(boxed, |d| edit_box(d, Some(focused), theme))
                    .child(
                        Textarea::new(&c.body)
                            .appearance(false)
                            .p_0()
                            .mx(px(-TEXTAREA_PX))
                            .when(boxed, |t| t.my(px(-TEXTAREA_PY)))
                            .text_size(px(14.))
                            .line_height(relative(1.5))
                            .disabled(locked),
                    ),
            )
            .when(!self.can_submit(), |d| {
                d.child(div().text_sm().text_color(muted).child(
                    "Saved locally. Load this PR with permission to post before sending to GitHub.",
                ))
            })
            .when(self.description_changed(), |d| {
                d.child(
                    v_flex()
                        .gap_2()
                        .text_sm()
                        .child("GitHub’s description changed. Compare the latest version above with your draft.")
                        .child(
                            h_flex().child(
                                Button::new("git-description-replace")
                                    .label("Replace with my draft…")
                                    .small()
                                    .disabled(locked || self.posting || self.loading)
                                    .on_click(cx.listener(|s, _, w, cx| s.replace_description(w, cx))),
                            ),
                        ),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("git-editor-save")
                            .loading(sending)
                            .label(if sending { "Saving…" } else { label })
                            .tooltip(format!("{label} on {} (cmd-enter)", self.host()))
                            .primary()
                            .small()
                            .disabled(
                                locked || self.posting || self.loading || !self.can_submit(),
                            )
                            .on_click(cx.listener(|s, _, w, cx| s.submit(w, cx))),
                    )
                    .child(
                        Button::new("git-editor-cancel")
                            .label("Keep draft")
                            .tooltip("Close and keep the local draft (esc)")
                            .ghost()
                            .small()
                            .on_click(cx.listener(|s, _, w, cx| {
                                s.close_editor(cx);
                                s.focus.focus(w, cx);
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_xs()
                            .text_color(faint)
                            .child("⌘↵ save · esc keep draft"),
                    )
                    .child(
                        Button::new("git-editor-discard")
                            .label("Discard")
                            .tooltip("Delete the local draft")
                            .ghost()
                            .small()
                            .disabled(locked)
                            .on_click(
                                cx.listener(|s, _, w, cx| s.confirm_discard_editor(w, cx)),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// Whether the open editor sits in the conversation: its PR is shown and so is its item.
    fn placed(&self) -> bool {
        let Some(c) = &self.composer else {
            return false;
        };
        !self.show_drafts
            && self.editing(&c.kind)
            && match &c.kind {
                Editing::Description => true,
                Editing::Comment => self.rows.contains(&Row::Composer),
                // A collapsed resolved thread does not draw its reply editor.
                Editing::Reply(id) => self.details.as_ref().is_some_and(|d| {
                    (d.threads.iter())
                        .position(|t| t.id == *id)
                        .is_some_and(|i| {
                            self.rows.contains(&Row::Thread(i))
                                && (!d.threads[i].resolved || self.expanded.contains(id))
                        })
                }),
            }
    }

    /// A draft opened from Local drafts, or one whose PR or thread is not on screen: the editor
    /// pinned below the pane, naming what it belongs to.
    fn pinned_editor(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let c = self.composer.as_ref().filter(|_| {
            !self.placed() && self.reader.is_none() && self.view == GitView::Conversation
        })?;
        let locked = self.archiving || self.sending.as_ref() == Some(&c.kind);
        let body = self.editor_body(false, window, cx);
        let theme = cx.theme();
        Some(
            v_flex()
                .id("git-composer")
                .test_support()
                .w_full()
                .max_w(px(680.))
                .mx_auto()
                .p_3()
                .gap_2()
                .border_1()
                .border_color(theme.border)
                .rounded(theme.radius_lg)
                .bg(theme.secondary)
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(match &c.kind {
                            Editing::Description => "Edit PR description",
                            Editing::Comment => "Comment on the PR",
                            Editing::Reply(_) => "Reply to review thread",
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(c.target.clone()),
                )
                .when(c.kind == Editing::Description, |d| {
                    d.child(
                        div()
                            .id("git-editor-title")
                            .test_support()
                            .child(Textarea::new(&c.title).disabled(locked)),
                    )
                })
                .child(body)
                .into_any_element(),
        )
    }

    /// The editor of `kind`, if it is the open one for the shown PR.
    fn editing(&self, kind: &Editing) -> bool {
        self.composer.as_ref().is_some_and(|c| {
            c.kind == *kind
                && self
                    .details
                    .as_ref()
                    .is_some_and(|d| d.target.url() == c.target)
        })
    }

    /// Title, meta line and description, editable in place.
    fn header(&mut self, ring: bool, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let d = self.details.as_ref().unwrap();
        let theme = cx.theme();
        let (muted, faint_c, hover, primary, active, transparent) = (
            theme.muted_foreground,
            faint(theme),
            theme.list_hover,
            theme.primary,
            theme.list_active,
            theme.transparent,
        );
        let mono = theme.mono_font_family.clone();
        let (state_color, state_icon) = match d.state.as_str() {
            "MERGED" => (muted, IconName::GitMerge),
            "CLOSED" => (theme.danger_foreground, IconName::GitPullRequestClosed),
            _ if d.draft => (muted, IconName::GitPullRequestDraft),
            _ => (theme.success_foreground, IconName::GitPullRequest),
        };
        let state = if d.draft && d.state == "OPEN" {
            "Draft"
        } else {
            state_label(&d.state)
        };
        let work_items = self.work_items_view(cx);
        let target = d.target.url();
        let can_edit = d.can_update || self.has_draft(&target, &Editing::Description);
        let (title, body, url, number, author, created, branches) = (
            d.title.clone(),
            d.body.clone(),
            d.url.clone(),
            d.target.number,
            d.author.clone(),
            d.created_at.clone(),
            format!("{} ← {}", d.base, d.head),
        );
        let editing = self.editing(&Editing::Description) && !self.show_drafts;
        // An opened but unchanged description is not worth a hint.
        let resume = !editing && self.has_draft(&target, &Editing::Description);
        let meta = h_flex()
            .gap_x_2()
            .gap_y_1()
            .flex_wrap()
            .text_xs()
            .text_color(muted)
            .child(
                h_flex()
                    .gap_1()
                    .text_color(state_color)
                    .child(Icon::new(state_icon).with_size(px(12.)))
                    .child(state.to_string()),
            )
            .child(format!("#{number}"))
            .child(
                h_flex()
                    .gap_1()
                    .child(format!("{} opened", self.display_name(&author)))
                    .when(!created.is_empty(), |d| {
                        d.child(time_label("git-pr-created", &created))
                    }),
            )
            .child(
                h_flex()
                    .gap_1()
                    .min_w_0()
                    .child(
                        Icon::new(IconName::GitBranch)
                            .with_size(px(12.))
                            .flex_none(),
                    )
                    .child(div().min_w_0().truncate().font_family(mono).child(branches)),
            )
            .when(resume, |d| {
                d.child(
                    Button::new("git-description-resume")
                        .icon(IconName::Pencil)
                        .label("Unsaved draft")
                        .tooltip("Resume editing your local draft (e)")
                        .ghost()
                        .xsmall()
                        .disabled(self.archiving)
                        .on_click(cx.listener(|s, _, w, cx| s.edit_description(false, w, cx))),
                )
            })
            .child(div().flex_1())
            .child(
                Button::new("git-open-pr")
                    .icon(IconName::ExternalLink)
                    .tooltip(format!("Open in {}", self.host()))
                    .ghost()
                    .xsmall()
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            );
        let clickable = can_edit && !self.archiving;
        // View and edit share one box (`edit_box`): only its border color changes, so
        // entering edit mode moves nothing.
        let content = if editing {
            let c = self.composer.as_ref().unwrap();
            let locked = self.archiving || self.sending.as_ref() == Some(&c.kind);
            let title_focused = c.title.read(cx).focus_handle(cx).is_focused(window);
            let theme = cx.theme();
            v_flex()
                .gap_3()
                .child(
                    edit_box(
                        div().id("git-editor-title").test_support(),
                        Some(title_focused),
                        theme,
                    )
                    // Enter moves on to the description instead of breaking the title.
                    .capture_action(cx.listener(|s, a: &Enter, w, cx| {
                        if !a.secondary {
                            cx.stop_propagation();
                            if let Some(c) = &s.composer {
                                c.body.update(cx, |b, cx| b.focus(w, cx));
                            }
                        }
                    }))
                    .child(
                        Textarea::new(&c.title)
                            .appearance(false)
                            .p_0()
                            .mx(px(-TEXTAREA_PX))
                            .my(px(-TEXTAREA_PY))
                            .text_size(px(24.))
                            .line_height(relative(1.25))
                            .font_weight(FontWeight::SEMIBOLD)
                            .disabled(locked),
                    ),
                )
                .child(meta)
                .child(self.editor_body(true, window, cx))
                .into_any_element()
        } else {
            let theme = cx.theme();
            v_flex()
                .gap_3()
                .child(
                    edit_box(div().id("git-pr-title").test_support(), None, theme)
                        .text_size(px(24.))
                        .line_height(relative(1.25))
                        .font_weight(FontWeight::SEMIBOLD)
                        .when(clickable, |d| {
                            d.cursor_text()
                                .hover(move |d| d.bg(hover))
                                .on_click(cx.listener(|s, ev: &ClickEvent, w, cx| {
                                    if plain_click(ev) {
                                        s.edit_description(true, w, cx);
                                    }
                                }))
                        })
                        .child(title),
                )
                .child(meta)
                .child(
                    edit_box(div().id("git-description").test_support(), None, theme)
                        .when(clickable, |d| {
                            d.cursor_text()
                                .hover(move |d| d.bg(hover))
                                // Links and buttons inside stop the click, so it never edits.
                                .on_click(cx.listener(|s, ev: &ClickEvent, w, cx| {
                                    if plain_click(ev) {
                                        s.edit_description(false, w, cx);
                                    }
                                }))
                        })
                        .child(if body.trim().is_empty() {
                            div()
                                .id("git-empty-description")
                                .test_support()
                                .text_color(if clickable { faint_c } else { muted })
                                .child(if clickable {
                                    "Add description…"
                                } else {
                                    "No description added yet."
                                })
                                .into_any_element()
                        } else {
                            self.body_view("description".into(), body, cx)
                        }),
                )
                .into_any_element()
        };
        v_flex()
            .id("git-overview")
            .test_support()
            .pt_6()
            .child(
                div()
                    .mx(px(-9.))
                    .px_2()
                    .py_1()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(if ring { primary } else { transparent })
                    .when(ring, |d| d.bg(active))
                    .child(content),
            )
            .child(
                div()
                    .pt_8()
                    .pb_1()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(muted)
                    .child("Activity"),
            )
            .when_some(work_items, |d, w| d.child(div().pt_6().child(w)))
            .into_any_element()
    }

    /// A comment's header (avatar, name, badges, time, hover actions) and body.
    fn comment_block(
        &mut self,
        c: &api::Comment,
        state: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = cx.theme();
        let (muted, fg, border) = (theme.muted_foreground, theme.foreground, theme.border);
        let state_color = match state {
            Some("APPROVED") => theme.success_foreground,
            Some("CHANGES_REQUESTED") => theme.warning_foreground,
            _ => muted,
        };
        let avatar = avatar(&c.author, c.bot, theme);
        let url = c.url.clone();
        let body =
            (!c.body.trim().is_empty()).then(|| self.body_view(c.id.clone(), c.body.clone(), cx));
        v_flex()
            .gap_2()
            .min_w_0()
            .child(
                h_flex()
                    .gap_2()
                    .h(px(20.))
                    .text_xs()
                    .text_color(muted)
                    .child(avatar)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(fg)
                            .truncate()
                            .child(self.display_name(&c.author)),
                    )
                    .when(c.bot, |d| {
                        d.child(
                            div()
                                .px_1()
                                .rounded(px(4.))
                                .border_1()
                                .border_color(border)
                                .line_height(px(14.))
                                .child("bot"),
                        )
                    })
                    .when_some(state, |d, state| {
                        d.child(
                            h_flex()
                                .gap_1()
                                .text_color(state_color)
                                .child(Icon::new(review_icon(state)).with_size(px(12.)))
                                .child(state_label(state).to_string()),
                        )
                    })
                    .child(time_label(
                        SharedString::from(format!("git-time-{}", c.id)),
                        &c.created_at,
                    ))
                    .child(div().flex_1())
                    // Hover actions sit in a fixed slot, so showing them moves nothing.
                    .child(crate::ui::copy_button(
                        SharedString::from(format!("git-copy-{}", c.id)),
                        c.body.clone(),
                        "git-row",
                    ))
                    .when(!url.is_empty(), |d| {
                        d.child(
                            div()
                                .flex_none()
                                .invisible()
                                .group_hover("git-row", |s| s.visible())
                                .child(
                                    Button::new(SharedString::from(format!("git-link-{}", c.id)))
                                        .icon(IconName::ExternalLink)
                                        .tooltip(format!("Open in {}", self.host()))
                                        .ghost()
                                        .xsmall()
                                        .on_click(move |_, _, cx| cx.open_url(&url)),
                                ),
                        )
                    }),
            )
            .when_some(body, |d, body| d.child(div().pl(px(28.)).child(body)))
    }

    /// A one-line stand-in for something collapsed: a hidden comment or a resolved thread.
    /// `id` is what opens (`git-{kind}-{id}` names the row), `text` its label and detail.
    fn collapsed_row(
        &self,
        kind: &str,
        id: &str,
        icon: Icon,
        text: (String, String),
        ring: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (label, detail) = text;
        let toggle = id.to_string();
        let id = SharedString::from(format!("git-{kind}-{id}"));
        let theme = cx.theme();
        crate::ui::list_cursor_row(
            h_flex()
                .id(id)
                .test_support()
                .h(px(32.))
                .px_3()
                .gap_2()
                .rounded(theme.radius)
                .text_sm()
                .text_color(theme.muted_foreground)
                .cursor_pointer()
                .child(icon.with_size(px(14.)).flex_none())
                .child(div().flex_none().child(label))
                .child(div().flex_1().min_w_0().truncate().child(detail))
                .child(
                    Icon::new(IconName::ChevronRight)
                        .with_size(px(14.))
                        .flex_none(),
                )
                .on_click(cx.listener(move |s, _, _, cx| s.toggle_expanded(&toggle, cx))),
            ring,
            true,
            theme,
        )
        .into_any_element()
    }

    fn comment_card(
        &mut self,
        c: api::Comment,
        state: Option<String>,
        ring: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if c.minimized && !self.expanded.contains(&c.id) {
            return self.collapsed_row(
                "hidden",
                &c.id,
                Icon::new(IconName::EyeOff),
                (
                    format!("Hidden comment by {}", self.display_name(&c.author)),
                    String::new(),
                ),
                ring,
                cx,
            );
        }
        let block = self.comment_block(&c, state.as_deref(), cx);
        let theme = cx.theme();
        v_flex()
            .id(SharedString::from(format!("git-comment-{}", c.id)))
            .test_support()
            .group("git-row")
            // Markdown blocks end with 8px of their own.
            .px_3()
            .pt_3()
            .pb_1()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(if ring { theme.primary } else { theme.border })
            .bg(theme.secondary)
            .child(block)
            .into_any_element()
    }

    /// A review without a body: "@x approved · 2d ago".
    fn review_event(&self, i: usize, ring: bool, cx: &mut Context<Self>) -> AnyElement {
        let r = &self.details.as_ref().unwrap().reviews[i];
        let theme = cx.theme();
        let color = match r.state.as_str() {
            "APPROVED" => theme.success_foreground,
            "CHANGES_REQUESTED" => theme.warning_foreground,
            _ => theme.muted_foreground,
        };
        let verb = match r.state.as_str() {
            "APPROVED" => "approved",
            "CHANGES_REQUESTED" => "requested changes",
            "DISMISSED" => "reviewed (dismissed)",
            "PENDING" => "has a pending review",
            _ => "reviewed",
        };
        crate::ui::list_cursor_row(
            h_flex()
                .id(("git-review", i))
                .test_support()
                .h(px(28.))
                .px_3()
                .gap_2()
                .rounded(theme.radius)
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(
                    Icon::new(review_icon(&r.state))
                        .with_size(px(14.))
                        .text_color(color)
                        .flex_none(),
                )
                .child(
                    div()
                        .text_color(theme.foreground)
                        .font_weight(FontWeight::MEDIUM)
                        .child(self.display_name(&r.comment.author)),
                )
                .child(verb)
                .child("·")
                .child(time_label(("git-review-time", i), &r.comment.created_at)),
            ring,
            true,
            theme,
        )
        .into_any_element()
    }

    /// A review thread as one chat card: file, diff context, comments, and an inline reply.
    fn thread_card(
        &mut self,
        i: usize,
        ring: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let d = self.details.as_ref().unwrap();
        let t = d.threads[i].clone();
        let target = d.target.url();
        let place = format!(
            "{}{}",
            if t.path.is_empty() {
                "Conversation"
            } else {
                &t.path
            },
            t.line
                .or(t.original_line)
                .map(|l| format!(":{l}"))
                .unwrap_or_default()
        );
        let first = t.comments.first();
        if t.resolved && !self.expanded.contains(&t.id) {
            let line = first
                .and_then(|c| c.body.lines().find(|l| !l.trim().is_empty()))
                .unwrap_or_default()
                .to_string();
            let theme = cx.theme();
            return self.collapsed_row(
                "thread-collapsed",
                &t.id,
                Icon::new(IconName::Check).text_color(theme.success_foreground),
                (
                    match &t.resolved_by {
                        Some(who) => format!("Resolved by @{who} · {place}"),
                        None => format!("Resolved · {place}"),
                    },
                    line,
                ),
                ring,
                cx,
            );
        }
        let hunk = first.and_then(|c| c.diff_hunk.clone());
        let reply = Editing::Reply(t.id.clone());
        let replying = self.editing(&reply);
        let draft = (self.store.drafts.get(&target))
            .and_then(|d| d.replies.get(&t.id))
            .filter(|b| !b.is_empty())
            .and_then(|b| b.lines().find(|l| !l.trim().is_empty()))
            .map(str::to_string);
        let can_reply = (t.can_reply || draft.is_some()) && !self.archiving;
        let can_toggle = if t.resolved {
            t.can_unresolve
        } else {
            t.can_resolve
        };
        let writing = self.posting || self.loading || self.archiving;
        let repo = self.task.repos[self.selected].name.clone();
        let comments: Vec<AnyElement> = t
            .comments
            .iter()
            .map(|c| {
                if c.minimized && !self.expanded.contains(&c.id) {
                    let id = c.id.clone();
                    let theme = cx.theme();
                    h_flex()
                        .id(SharedString::from(format!("git-hidden-{}", c.id)))
                        .test_support()
                        .px_3()
                        .py_2()
                        .gap_2()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .cursor_pointer()
                        .hover(|d| d.bg(theme.secondary_hover))
                        .child(Icon::new(IconName::EyeOff).with_size(px(14.)))
                        .child(format!(
                            "Hidden comment by {}",
                            self.display_name(&c.author)
                        ))
                        .on_click(cx.listener(move |s, _, _, cx| s.toggle_expanded(&id, cx)))
                        .into_any_element()
                } else {
                    let block = self.comment_block(c, None, cx);
                    div()
                        .group("git-row")
                        .px_3()
                        .pt_3()
                        .pb_1()
                        .child(block)
                        .into_any_element()
                }
            })
            .collect();
        let reply_area = if replying {
            Some(
                // The reply's text lines up with the comment bodies above it.
                div()
                    .pl(px(40.))
                    .pr_3()
                    .py_3()
                    .child(self.editor_body(true, window, cx))
                    .into_any_element(),
            )
        } else if can_reply {
            let id = t.id.clone();
            let theme = cx.theme();
            Some(
                h_flex()
                    .id(SharedString::from(format!("git-thread-reply-{}", t.id)))
                    .test_support()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .text_sm()
                    .cursor_text()
                    .hover(|d| d.bg(theme.secondary_hover))
                    .map(|d| match draft {
                        Some(line) => d
                            .text_color(theme.muted_foreground)
                            .child(Icon::new(IconName::Pencil).with_size(px(14.)).flex_none())
                            .child(div().flex_none().child("Draft ·"))
                            .child(div().min_w_0().truncate().child(line)),
                        None => d.text_color(faint(theme)).child("Leave a reply…"),
                    })
                    .on_click(cx.listener(move |s, _, w, cx| s.start_reply(&id, w, cx)))
                    .into_any_element(),
            )
        } else {
            None
        };
        let theme = cx.theme();
        let (border, muted, mono, mono_size) = (
            theme.border,
            theme.muted,
            theme.mono_font_family.clone(),
            theme.mono_font_size,
        );
        let (added, removed, quiet) = (
            theme.success_foreground,
            theme.danger_foreground,
            theme.muted_foreground,
        );
        let path = t.path.clone();
        let (id, resolved) = (t.id.clone(), t.resolved);
        let separated = |items: Vec<AnyElement>| {
            items.into_iter().enumerate().map(move |(j, e)| {
                div()
                    .when(j > 0, |d| d.border_t_1().border_color(border))
                    .child(e)
            })
        };
        v_flex()
            .id(("git-thread", i))
            .test_support()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(if ring { theme.primary } else { border })
            .bg(theme.secondary)
            .overflow_hidden()
            .child(
                h_flex()
                    .px_1()
                    .py_1()
                    .gap_1()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        Button::new(("git-thread-path", i))
                            .tooltip(format!("Show file in Diff: {place}"))
                            .accessibility_label(place.clone())
                            .icon(IconName::FileCode)
                            // Narrow, the folders give way; `file:line` always shows.
                            .child({
                                let (dir, file) = match place.rfind('/') {
                                    Some(at) => place.split_at(at + 1),
                                    None => ("", place.as_str()),
                                };
                                h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().min_w_0().truncate().child(dir.to_string()))
                                    .child(div().flex_none().child(file.to_string()))
                            })
                            .flex_1()
                            .min_w_0()
                            .justify_start()
                            .ghost()
                            .small()
                            .on_click(cx.listener(move |_, _, _, cx| {
                                if !path.is_empty() {
                                    cx.emit(GitFeatureEvent::OpenFile {
                                        repo: repo.clone(),
                                        path: path.clone(),
                                    })
                                }
                            })),
                    )
                    .when(t.outdated, |d| {
                        d.child(
                            div()
                                .px_1()
                                .rounded(px(4.))
                                .border_1()
                                .border_color(border)
                                .text_xs()
                                .text_color(quiet)
                                .child("Outdated"),
                        )
                    })
                    .when(can_toggle, |d| {
                        let id = id.clone();
                        d.child(
                            Button::new(SharedString::from(format!("git-thread-resolve-{}", t.id)))
                                .label(if resolved { "Reopen" } else { "Resolve" })
                                .icon(if resolved {
                                    IconName::RotateCcw
                                } else {
                                    IconName::Check
                                })
                                .ghost()
                                .small()
                                .disabled(writing)
                                .on_click(
                                    cx.listener(move |s, _, w, cx| {
                                        s.resolve(&id, !resolved, w, cx)
                                    }),
                                ),
                        )
                    })
                    .when(resolved, |d| {
                        let id = id.clone();
                        d.child(
                            Button::new(SharedString::from(format!(
                                "git-thread-collapse-{}",
                                t.id
                            )))
                            .icon(IconName::ChevronDown)
                            .tooltip("Collapse resolved thread")
                            .ghost()
                            .small()
                            .on_click(cx.listener(move |s, _, _, cx| s.toggle_expanded(&id, cx))),
                        )
                    }),
            )
            .when_some(hunk, |d, hunk| {
                let lines = hunk_tail(&hunk, 4);
                d.child(
                    v_flex()
                        .px_3()
                        .py_2()
                        .bg(muted)
                        .border_b_1()
                        .border_color(border)
                        .font_family(mono)
                        .text_size(mono_size)
                        .children(lines.into_iter().map(|l| {
                            div()
                                .truncate()
                                .text_color(match l.as_bytes().first() {
                                    Some(b'+') => added,
                                    Some(b'-') => removed,
                                    _ => quiet,
                                })
                                .child(l)
                        })),
                )
            })
            .children(separated(comments))
            .when_some(reply_area, |d, area| {
                d.child(div().border_t_1().border_color(border).child(area))
            })
            .into_any_element()
    }

    /// The comment composer: a quiet "Leave a comment…" card that opens into the editor.
    fn composer_row(
        &mut self,
        ring: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let editing = self.editing(&Editing::Comment);
        let target = self
            .details
            .as_ref()
            .map(|d| d.target.url())
            .unwrap_or_default();
        let draft = (self.store.drafts.get(&target))
            .map(|d| d.note.clone())
            .filter(|n| !n.is_empty());
        let focused = editing
            && self
                .composer
                .as_ref()
                .is_some_and(|c| c.body.read(cx).focus_handle(cx).is_focused(window));
        let body = editing.then(|| self.editor_body(false, window, cx));
        let theme = cx.theme();
        let card = v_flex()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(if ring || focused {
                theme.primary
            } else {
                theme.border
            })
            .bg(theme.secondary);
        match body {
            Some(body) => card
                .id("git-composer")
                .test_support()
                .p_3()
                .child(body)
                .into_any_element(),
            None => card
                .id("git-comment-new")
                .test_support()
                .h(px(40.))
                .px_3()
                .justify_center()
                .text_sm()
                .cursor_text()
                .hover(|d| d.bg(theme.secondary_hover))
                .map(|d| match draft {
                    Some(note) => d.child(
                        h_flex()
                            .gap_2()
                            .text_color(theme.muted_foreground)
                            .child(Icon::new(IconName::Pencil).with_size(px(14.)).flex_none())
                            .child(div().flex_none().child("Draft ·"))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .child(note.lines().next().unwrap_or_default().to_string()),
                            ),
                    ),
                    None => d.text_color(faint(theme)).child("Leave a comment…"),
                })
                .when(!self.archiving, |d| {
                    d.on_click(cx.listener(|s, _, w, cx| s.start_comment(w, cx)))
                })
                .into_any_element(),
        }
    }

    fn render_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row) = self.rows.get(index).cloned() else {
            return div().into_any_element();
        };
        let Some(details) = &self.details else {
            return div().into_any_element();
        };
        let ring = index == self.cursor && self.kbd_cursor && self.focus.is_focused(window);
        let (content, top) = match row {
            Row::Header => (self.header(ring, window, cx), px(0.)),
            Row::Comment(i) => {
                let c = details.comments[i].clone();
                (self.comment_card(c, None, ring, cx), px(12.))
            }
            Row::Review(i) => {
                let r = details.reviews[i].clone();
                if r.comment.body.trim().is_empty() {
                    (self.review_event(i, ring, cx), px(8.))
                } else {
                    (
                        self.comment_card(r.comment, Some(r.state), ring, cx),
                        px(12.),
                    )
                }
            }
            Row::Thread(i) => (self.thread_card(i, ring, window, cx), px(12.)),
            Row::Empty => (
                div()
                    .id("git-empty-activity")
                    .test_support()
                    .px_3()
                    .text_sm()
                    .text_color(faint(cx.theme()))
                    .child("No comments or reviews yet")
                    .into_any_element(),
                px(4.),
            ),
            Row::Composer => (self.composer_row(ring, window, cx), px(24.)),
        };
        div()
            .id(("git-row", index))
            .test_support()
            .w_full()
            .px_4()
            .pt(top)
            .when(index + 1 == self.rows.len(), |d| d.pb_8())
            .text_size(px(14.))
            .line_height(relative(1.5))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |s, _, _, cx| {
                    if s.cursor != index || s.kbd_cursor {
                        s.cursor = index;
                        s.kbd_cursor = false;
                        cx.notify();
                    }
                }),
            )
            .child(div().w_full().max_w(px(680.)).mx_auto().child(content))
            .into_any_element()
    }

    /// Arrows and j/k: move the cursor, or scroll the reader.
    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.reader.is_some() {
            self.scroll(delta as f32 * 2., cx);
        } else {
            self.kbd_cursor = true;
            self.move_cursor(delta, cx);
        }
    }

    /// Whether the conversation list itself is on screen (not Commits, drafts or the reader).
    fn conversation_shown(&self) -> bool {
        self.view == GitView::Conversation && self.reader.is_none() && !self.show_drafts
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

    /// Scrolls so the end of item `ix` (where "Show more" sits) is on screen.
    /// Focus back on the pane itself, as when the open editor's field leaves the list.
    #[cfg(test)]
    pub(crate) fn focus_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
    }

    #[cfg(test)]
    pub(crate) fn scroll_row_end(&mut self, ix: usize, cx: &mut Context<Self>) {
        let height = self.list.viewport_bounds().size.height;
        self.list.scroll_to(ListOffset {
            item_ix: ix + 1,
            offset_in_item: px(0.),
        });
        self.list.scroll_by(-(height - px(60.)).max(px(0.)));
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

/// The conversation in time order: comments, reviews and threads by creation time, each thread
/// right after the review it was submitted with. Reviews that say nothing (no body, no
/// threads, just "commented", as GitHub records for every web reply) are left out.
fn timeline(d: &api::Details, composer: bool) -> Vec<Row> {
    let review_of = |t: &api::Thread| t.comments.first().and_then(|c| c.review_id.clone());
    let reviewed: HashSet<String> = d.threads.iter().filter_map(review_of).collect();
    let mut items: Vec<(&str, Row)> = (d.comments.iter().enumerate())
        .map(|(i, c)| (c.created_at.as_str(), Row::Comment(i)))
        .collect();
    items.extend(
        (d.reviews.iter().enumerate())
            .filter(|(_, r)| {
                !r.comment.body.trim().is_empty()
                    || r.state != "COMMENTED"
                    || reviewed.contains(&r.comment.id)
            })
            .map(|(i, r)| (r.comment.created_at.as_str(), Row::Review(i))),
    );
    let ids: HashSet<&str> = d.reviews.iter().map(|r| r.comment.id.as_str()).collect();
    items.extend(
        (d.threads.iter().enumerate())
            .filter(|(_, t)| !review_of(t).is_some_and(|id| ids.contains(id.as_str())))
            .map(|(i, t)| {
                let at = t.comments.first().map_or("", |c| c.created_at.as_str());
                (at, Row::Thread(i))
            }),
    );
    // GitHub's timestamps are all UTC `Z`, so text order is time order; the sort is stable.
    items.sort_by(|a, b| a.0.cmp(b.0));
    let mut rows = vec![Row::Header];
    for (_, row) in items {
        rows.push(row.clone());
        if let Row::Review(i) = row {
            let id = &d.reviews[i].comment.id;
            rows.extend(
                (d.threads.iter().enumerate())
                    .filter(|(_, t)| review_of(t).as_ref() == Some(id))
                    .map(|(j, _)| Row::Thread(j)),
            );
        }
    }
    if rows.len() == 1 {
        rows.push(Row::Empty);
    }
    if composer {
        rows.push(Row::Composer);
    }
    rows
}

/// A row's identity across reloads: `header`, `comment:C_1`, `review:R_1`, `thread:T_0`,
/// `empty`, `composer`.
fn row_key(d: &api::Details, row: &Row) -> String {
    match row {
        Row::Header => "header".into(),
        Row::Comment(i) => format!("comment:{}", d.comments[*i].id),
        Row::Review(i) => format!("review:{}", d.reviews[*i].comment.id),
        Row::Thread(i) => format!("thread:{}", d.threads[*i].id),
        Row::Empty => "empty".into(),
        Row::Composer => "composer".into(),
    }
}

/// Whether the message the reader shows (`description` or a comment id) is still there.
fn has_message(d: &api::Details, key: &str) -> bool {
    key == "description"
        || d.comments.iter().any(|c| c.id == key)
        || d.reviews.iter().any(|r| r.comment.id == key)
        || (d.threads.iter()).any(|t| t.comments.iter().any(|c| c.id == key))
}

/// The padding a multi-line field keeps inside itself (gpui-kit's medium input); pulled out
/// so edited text sits exactly where the text it replaces was read.
const TEXTAREA_PX: f32 = 10.;
const TEXTAREA_PY: f32 = 8.;

/// The box the title and description sit in, read or edited: `mx(-9px)` + border + `px_2`
/// keeps the text on the column edge. `focused` is `None` while reading (no border).
fn edit_box<E: Styled>(e: E, focused: Option<bool>, theme: &gpui_kit::component::Theme) -> E {
    e.mx(px(-9.))
        .px_2()
        .py_1()
        .border_1()
        .rounded(theme.radius)
        .border_color(match focused {
            None => theme.transparent,
            Some(true) => theme.primary,
            Some(false) => theme.border,
        })
}

/// A click that should start editing: a single left click, not the end of a text selection.
fn plain_click(ev: &ClickEvent) -> bool {
    match ev {
        ClickEvent::Mouse(m) => {
            let moved = m.up.position - m.down.position;
            ev.standard_click()
                && m.up.click_count <= 1
                && moved.x.abs() < px(4.)
                && moved.y.abs() < px(4.)
        }
        _ => ev.standard_click(),
    }
}

/// "3d ago" (the exact local time in its tooltip); unparseable times are shown as they are.
fn time_label(id: impl Into<ElementId>, iso: &str) -> Stateful<Div> {
    let (relative, exact) = match chrono::DateTime::parse_from_rfc3339(iso) {
        Ok(t) => {
            let age = (chrono::Utc::now() - t.with_timezone(&chrono::Utc))
                .to_std()
                .unwrap_or_default();
            (
                if age.as_secs() < 60 {
                    "just now".to_string()
                } else {
                    crate::sync::ago(age)
                },
                t.with_timezone(&chrono::Local)
                    .format("%b %-d, %Y at %H:%M")
                    .to_string(),
            )
        }
        Err(_) => (iso.to_string(), iso.to_string()),
    };
    let exact = SharedString::from(exact);
    div()
        .id(id)
        .flex_none()
        .child(relative)
        .tooltip(move |w, cx| {
            gpui_kit::component::tooltip::Tooltip::new(exact.clone()).build(w, cx)
        })
}

/// A 20px neutral circle with the author's initial (a bot glyph for apps).
fn avatar(name: &str, bot: bool, theme: &gpui_kit::component::Theme) -> Div {
    let initial: String = name.chars().take(1).flat_map(char::to_uppercase).collect();
    div()
        .size(px(20.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(theme.accent)
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.muted_foreground)
        .map(|d| match bot {
            true => d.child(Icon::new(IconName::Bot).with_size(px(12.))),
            false => d.child(initial),
        })
}

fn review_icon(state: &str) -> IconName {
    match state {
        "APPROVED" => IconName::Check,
        "CHANGES_REQUESTED" => IconName::CircleAlert,
        "PENDING" => IconName::Clock,
        "DISMISSED" => IconName::CircleMinus,
        _ => IconName::MessageSquare,
    }
}

/// The last `n` lines of a diff hunk, without its `@@` header.
fn hunk_tail(hunk: &str, n: usize) -> Vec<String> {
    let lines: Vec<&str> = hunk.lines().filter(|l| !l.starts_with("@@")).collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| l.to_string())
        .collect()
}

/// The selected repository's history source: its worktree and effective comparison base.
fn repo_context(task: &Task, selected: usize) -> Option<RepoContext> {
    let repo = task.repos.get(selected)?;
    Some(RepoContext {
        wt: task.dir().join(&repo.name),
        base: repo.base.clone(),
    })
}

impl GitFeature {
    /// Where the shown PR lives, for labels.
    fn host(&self) -> &'static str {
        match self.details.as_ref().and_then(|d| d.target.remote()) {
            Some(_) => "Azure DevOps",
            None => "GitHub",
        }
    }

    /// GitHub logins read as `@login`; Azure DevOps gives display names.
    fn display_name(&self, author: &str) -> String {
        match (
            author.is_empty(),
            self.details.as_ref().and_then(|d| d.target.remote()),
        ) {
            (true, _) => "Deleted user".into(),
            (false, Some(_)) => author.to_string(),
            (false, None) => format!("@{author}"),
        }
    }
}

impl GitFeature {
    /// The branch's PBIs, each with its tasks: state, who has it, and a link.
    fn work_items_view(&self, cx: &App) -> Option<AnyElement> {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let items = match &self.work_items {
            Ok(items) if items.is_empty() => return None,
            Ok(items) => items,
            Err(e) => {
                return Some(
                    div()
                        .id("git-work-items-error")
                        .test_support()
                        .text_sm()
                        .text_color(muted)
                        .child(format!("Work items unavailable: {e}"))
                        .into_any_element(),
                );
            }
        };
        let line = |w: &crate::azure::WorkItem, top: bool| {
            let url = w.url.clone();
            let who = match w.assigned.is_empty() {
                true => "Unassigned".to_string(),
                false => w.assigned.clone(),
            };
            h_flex()
                .gap_2()
                .min_w_0()
                .text_size(px(if top { 14. } else { 13. }))
                .child(
                    Icon::new(match () {
                        _ if top => IconName::ClipboardList,
                        _ if w.done() => IconName::CircleCheck,
                        _ => IconName::Circle,
                    })
                    .with_size(px(14.))
                    .flex_none()
                    .text_color(if !top && w.done() {
                        theme.success_foreground
                    } else {
                        muted
                    }),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(muted)
                        .child(format!("{} {}", w.kind, w.id)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .when(top, |d| d.font_weight(FontWeight::MEDIUM))
                        .child(w.title.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .px_2()
                        .rounded(theme.radius)
                        .bg(theme.muted)
                        .text_xs()
                        .text_color(if w.done() {
                            theme.success_foreground
                        } else {
                            theme.foreground
                        })
                        .child(w.state.clone()),
                )
                .child(div().flex_none().text_xs().text_color(muted).child(who))
                .child(
                    Button::new(SharedString::from(format!("git-work-item-{}", w.id)))
                        .icon(IconName::ExternalLink)
                        .tooltip("Open in Azure DevOps")
                        .ghost()
                        .xsmall()
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                )
        };
        Some(
            v_flex()
                .id("git-work-items")
                .test_support()
                .w_full()
                .gap_3()
                .text_left()
                .child(div().text_xs().text_color(muted).child("Work items"))
                .children(items.iter().map(|w| {
                    let done = w.children.iter().filter(|c| c.done()).count();
                    v_flex()
                        .gap_1()
                        .p_3()
                        .rounded(theme.radius_lg)
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.secondary)
                        .child(line(w, true))
                        .when(!w.children.is_empty(), |d| {
                            d.child(
                                div()
                                    .pl_6()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format!("{done} of {} done", w.children.len())),
                            )
                        })
                        .children(
                            w.children
                                .iter()
                                .map(|c| div().pl_6().child(line(c, false))),
                        )
                }))
                .into_any_element(),
        )
    }
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
        let repo = self.task.repos.get(self.selected);
        let commits_view = self.view == GitView::Commits;
        h_flex()
            .flex_none()
            .gap_2()
            .px_4()
            .py_2()
            .flex_wrap()
            .border_b_1()
            .border_color(border)
            // The repo picker only when there is a choice.
            .when(self.task.repos.len() > 1, |d| {
                d.children(self.task.repos.iter().enumerate().map(|(i, r)| {
                    Button::new(SharedString::from(format!("git-repo-{i}")))
                        .label(r.name.clone())
                        .small()
                        .ghost()
                        .selected(i == self.selected)
                        .disabled(disabled)
                        .on_click(cx.listener(move |s, _, w, cx| s.select_repo(i, w, cx)))
                        .map(|b| crate::ui::toolbar(format!("git-repo-{i}"), b))
                }))
                .child(div().w(px(8.)))
            })
            .child(
                Button::new("git-view-conversation")
                    .label("Conversation")
                    .icon(IconName::MessageSquare)
                    .small()
                    .ghost()
                    .selected(!commits_view && !self.show_drafts)
                    .on_click(cx.listener(|s, _, w, cx| {
                        s.show_drafts = false;
                        s.set_view(GitView::Conversation, w, cx);
                        cx.notify();
                    }))
                    .map(|b| crate::ui::toolbar("git-view-conversation", b)),
            )
            .child(
                Button::new("git-view-commits")
                    .label("Commits")
                    .icon(IconName::GitBranch)
                    .small()
                    .ghost()
                    .selected(commits_view)
                    .on_click(cx.listener(|s, _, w, cx| s.set_view(GitView::Commits, w, cx)))
                    .map(|b| crate::ui::toolbar("git-view-commits", b)),
            )
            .child(div().flex_1())
            .when(!commits_view, |r| {
                r.when(drafts > 0 || self.show_drafts, |r| {
                    r.child(
                        Button::new("git-local-drafts")
                            .label(format!("Local drafts ({drafts})"))
                            .small()
                            .ghost()
                            .selected(self.show_drafts)
                            .disabled(self.archiving)
                            .on_click(cx.listener(|s, _, w, cx| {
                                s.show_drafts = !s.show_drafts;
                                s.reader = None;
                                s.focus.focus(w, cx);
                                cx.notify();
                            }))
                            .map(|b| crate::ui::toolbar("git-local-drafts", b)),
                    )
                })
                .child(
                    Button::new("git-refresh")
                        .icon(IconName::RefreshCw)
                        .loading(self.loading)
                        .accessibility_label(if self.loading {
                            "Refreshing…"
                        } else {
                            "Refresh"
                        })
                        .tooltip("Refresh pull request")
                        .small()
                        .ghost()
                        .disabled(disabled || self.loading || repo.is_none())
                        .on_click(cx.listener(|s, _, w, cx| s.refresh(w, cx)))
                        .map(|b| crate::ui::toolbar("git-refresh", b)),
                )
            })
            .into_any_element()
    }

    /// Icon, title and detail shown while there is no pull request to list.
    fn placeholder_state(&self) -> (&'static str, &'static str, &'static str, IconName) {
        if self.task.repos.get(self.selected).is_none() {
            (
                "git-empty-repositories",
                "No repositories in this task",
                "Attach a repository to view its pull request.",
                IconName::FolderGit2,
            )
        } else if self.loading {
            (
                "git-loading",
                "Loading pull request…",
                "Fetching the description and review activity.",
                IconName::GitPullRequest,
            )
        } else if self.error.is_some() {
            (
                "git-load-error",
                "Could not load the pull request",
                "Check the connection and your gh or az login, then try again.",
                IconName::TriangleAlert,
            )
        } else if !self.loaded {
            (
                "git-unloaded",
                "Pull request not loaded",
                if self.archiving {
                    "This task is being deleted."
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
        let work_items = self.work_items_view(cx);
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let disabled = self.posting || self.archiving;
        let repo = self.task.repos.get(self.selected);
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
            .when_some(work_items, |d, w| {
                d.child(div().w_full().max_w(px(680.)).mx_auto().pt_8().child(w))
            })
            .into_any_element()
    }

    fn drafts_list(&self, drafts: Vec<(String, Editing)>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, border, secondary, radius) = (
            theme.muted_foreground,
            theme.border,
            theme.secondary,
            theme.radius_lg,
        );
        let disabled = self.archiving;
        let rows: Vec<_> = drafts
            .into_iter()
            .enumerate()
            .map(|(i, (target, kind))| {
                let draft = self.store.drafts.get(&target);
                let (label, text) = match &kind {
                    Editing::Description => (
                        "Resume description",
                        draft
                            .and_then(|d| d.description.as_ref())
                            .map(|d| d.title.clone()),
                    ),
                    Editing::Comment => ("Resume comment", draft.map(|d| d.note.clone())),
                    Editing::Reply(id) => (
                        "Resume reply",
                        draft.and_then(|d| d.replies.get(id).cloned()),
                    ),
                };
                // Where it goes: the thread's file while that PR is shown.
                let place = match &kind {
                    Editing::Reply(id) => (self.details.as_ref())
                        .filter(|d| d.target.url() == target)
                        .and_then(|d| d.threads.iter().find(|t| t.id == *id))
                        .map(|t| format!(" · {}", t.path)),
                    _ => None,
                };
                let preview: String = (text.unwrap_or_default().lines())
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or_default()
                    .chars()
                    .take(120)
                    .collect();
                v_flex()
                    .gap_1()
                    .p_3()
                    .rounded(radius)
                    .border_1()
                    .border_color(border)
                    .bg(secondary)
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .truncate()
                            .child(format!("{target}{}", place.unwrap_or_default())),
                    )
                    .child(div().text_sm().truncate().child(preview))
                    .child(
                        h_flex().child(
                            Button::new(("git-local-draft", i))
                                .label(label)
                                .small()
                                .disabled(disabled)
                                .on_click(cx.listener(move |s, _, w, cx| {
                                    s.open_draft(target.clone(), kind.clone(), false, w, cx)
                                })),
                        ),
                    )
            })
            .collect();
        let empty = rows.is_empty();
        div()
            .id("git-local-drafts-list")
            .test_support()
            .size_full()
            .overflow_y_scroll()
            .px_4()
            .py_6()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(680.))
                    .mx_auto()
                    .gap_3()
                    .text_size(px(14.))
                    .child(
                        div().text_sm().text_color(muted).child(
                            "Saved on this computer. Open a draft to edit, copy or discard it.",
                        ),
                    )
                    .when(empty, |d| d.child("No local drafts"))
                    .children(rows),
            )
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (warning, muted) = (theme.warning_foreground, theme.muted_foreground);
        match self.reveal_pending {
            Some((ix, top, false)) => {
                self.reveal_pending = Some((ix, top, true));
                cx.notify();
            }
            Some((ix, top, true)) => {
                self.reveal_pending = None;
                self.reveal(ix, top);
            }
            None => {}
        }
        let drafts = self.local_drafts();
        let toolbar = self.toolbar(drafts.len(), cx);
        let pinned = self.pinned_editor(window, cx);
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
        v_flex()
            .id("git-feature")
            .key_context("GitFeature")
            .on_action(cx.listener(|s, _: &SaveFile, w, cx| s.submit(w, cx)))
            .on_action(cx.listener(|s, _: &SelectNext, _, cx| s.step(1, cx)))
            .on_action(cx.listener(|s, _: &SelectPrev, _, cx| s.step(-1, cx)))
            .on_action(cx.listener(|s, _: &PageDown, _, cx| s.scroll(15., cx)))
            .on_action(cx.listener(|s, _: &PageUp, _, cx| s.scroll(-15., cx)))
            .on_action(cx.listener(|s, _: &SelectFirst, _, cx| {
                match &s.reader {
                    Some(r) => r.doc.read(cx).scroll_to_start(),
                    None => {
                        s.kbd_cursor = true;
                        s.set_cursor(0, cx);
                    }
                }
                cx.notify();
            }))
            .on_action(cx.listener(|s, _: &SelectLast, _, cx| {
                match &s.reader {
                    Some(r) => r.doc.read(cx).scroll_to_end(),
                    None => {
                        s.kbd_cursor = true;
                        s.set_cursor(s.rows.len().saturating_sub(1), cx);
                    }
                }
                cx.notify();
            }))
            // A text field lets Enter through after handling it; that is not the cursor's Enter.
            .on_action(cx.listener(|s, _: &Confirm, w, cx| {
                if s.conversation_shown() && s.focus.is_focused(w) && !s.return_to_editor(w, cx) {
                    s.kbd_cursor = true;
                    s.activate(w, cx);
                }
            }))
            .on_action(cx.listener(|s, _: &EditDescription, w, cx| {
                if s.conversation_shown() && !s.return_to_editor(w, cx) {
                    s.edit_description(false, w, cx);
                }
            }))
            .on_action(cx.listener(|s, _: &ReplyToThread, w, cx| {
                if s.conversation_shown() && !s.return_to_editor(w, cx) {
                    s.reply_at_cursor(w, cx);
                }
            }))
            .on_action(cx.listener(|s, _: &CommentOnPr, w, cx| {
                if s.conversation_shown() && !s.return_to_editor(w, cx) {
                    s.start_comment(w, cx);
                }
            }))
            // cmd-enter in any open editor saves or posts it.
            .capture_action(cx.listener(|s, a: &Enter, w, cx| {
                if a.secondary && s.composer.is_some() {
                    cx.stop_propagation();
                    s.submit(w, cx);
                }
            }))
            .on_action(cx.listener(|s, _: &CancelEdit, w, cx| {
                if s.reader.is_some() {
                    s.close_reader(w, cx);
                } else {
                    s.close_editor(cx);
                }
                // Escape at the commit list keeps focus there, so arrows still move it.
                match s.view {
                    GitView::Commits => s.commits.read(cx).focus_target(cx).focus(w, cx),
                    GitView::Conversation => s.focus.focus(w, cx),
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
                        div().w_full().px_4().child(
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
                            // In the reading column, not across the pane.
                            .w_full()
                            .max_w(px(680.))
                            .mx_auto()
                            .px_0()
                            .id("git-error")
                            .test_support(),
                        ),
                    )
                },
            )
            .child(div().flex_1().min_h_0().child(content))
            .when_some(pinned, |d, c| {
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
    pub cursor: usize,
    /// The conversation's items, as `Row` keys.
    pub rows: Vec<String>,
    /// The open editor's kind (`Description`, `Comment`, `Reply("T_0")`).
    pub editing: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr::Target;
    use api::{Comment, Details, Review, Thread};

    fn comment(id: &str, at: &str, review: Option<&str>) -> Comment {
        Comment {
            id: id.into(),
            body: format!("{id} body"),
            author: "a".into(),
            url: String::new(),
            created_at: at.into(),
            bot: false,
            minimized: false,
            diff_hunk: None,
            review_id: review.map(Into::into),
        }
    }

    fn thread(id: &str, first: Comment) -> Thread {
        Thread {
            id: id.into(),
            path: "a.rs".into(),
            line: Some(1),
            resolved: false,
            outdated: false,
            can_reply: true,
            can_resolve: true,
            can_unresolve: false,
            original_line: None,
            resolved_by: None,
            comments: vec![first],
        }
    }

    fn review(id: &str, at: &str, body: &str, state: &str) -> Review {
        Review {
            comment: Comment {
                body: body.into(),
                ..comment(id, at, None)
            },
            state: state.into(),
        }
    }

    fn keys(d: &Details, rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                Row::Header => "header".to_string(),
                Row::Comment(i) => d.comments[*i].id.clone(),
                Row::Review(i) => d.reviews[*i].comment.id.clone(),
                Row::Thread(i) => d.threads[*i].id.clone(),
                Row::Empty => "empty".into(),
                Row::Composer => "composer".into(),
            })
            .collect()
    }

    fn details() -> Details {
        Details {
            target: Target::from_url("https://github.com/acme/api/pull/7").unwrap(),
            id: "PR".into(),
            title: "t".into(),
            body: String::new(),
            url: String::new(),
            state: "OPEN".into(),
            author: "a".into(),
            created_at: String::new(),
            head: "h".into(),
            head_sha: None,
            base: "b".into(),
            draft: false,
            can_update: true,
            can_comment: true,
            comments: vec![],
            reviews: vec![],
            threads: vec![],
        }
    }

    #[::core::prelude::v1::test]
    fn timeline_is_chronological_with_threads_under_their_review() {
        let mut d = details();
        d.comments = vec![
            comment("C_late", "2026-10-03T00:00:00Z", None),
            comment("C_early", "2026-10-01T00:00:00Z", None),
        ];
        d.reviews = vec![
            review("R_1", "2026-10-02T00:00:00Z", "", "COMMENTED"),
            review("R_ok", "2026-10-04T00:00:00Z", "", "APPROVED"),
            // A web reply's empty review says nothing and owns no thread: left out.
            review("R_reply", "2026-10-05T00:00:00Z", "", "COMMENTED"),
        ];
        d.threads = vec![
            // Started before its review was submitted; still shown under it.
            thread("T_r", comment("TC_r", "2026-10-01T12:00:00Z", Some("R_1"))),
            thread(
                "T_loose",
                comment("TC_l", "2026-10-02T12:00:00Z", Some("R_gone")),
            ),
        ];
        assert_eq!(
            keys(&d, &timeline(&d, true)),
            [
                "header", "C_early", "R_1", "T_r", "T_loose", "C_late", "R_ok", "composer"
            ]
        );
        d.comments.clear();
        d.reviews.clear();
        d.threads.clear();
        assert_eq!(keys(&d, &timeline(&d, false)), ["header", "empty"]);
    }

    #[::core::prelude::v1::test]
    fn hunk_tail_drops_the_header() {
        assert_eq!(hunk_tail("@@ -1 +1 @@\n a\n-b\n+c", 4), [" a", "-b", "+c"]);
        assert_eq!(hunk_tail("1\n2\n3\n4\n5", 2), ["4", "5"]);
    }
}
