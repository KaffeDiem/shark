//! The Artifacts feature: the task's `.context` folder, where agents and the user keep files.
mod list;
pub(crate) mod preview;

use crate::features::differ::{
    copy_path_entry, copy_relative_entry, open_in_entries, reveal_entry,
};
use crate::media::Lru;
use crate::menu::{self, Entry};
use crate::ui::{Nav, confirm, copy, empty_state, faint, nav_actions, notify_err, quiet_edge};
use crate::{
    AttachArtifact, Confirm, CopyArtifactPath, DeleteArtifact, FilterArtifacts, PinArtifact,
    RefreshArtifacts,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::resizable::{
    ResizablePanelEvent, ResizableState, h_resizable, resizable_panel,
};
use gpui_kit::component::{ActiveTheme, v_flex};
use gpui_kit::*;
use list::{Item, Row, arrange, rows};
use preview::{Preview, PreviewEvent};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// A file modified this recently may be half written, so a scan leaves it for the next one.
const SETTLE: Duration = Duration::from_millis(300);
/// The scan runs this often while the pane is on screen.
const POLL: Duration = Duration::from_secs(2);
/// Loading shows only if the first scan takes longer than this.
const LOADING_AFTER: Duration = Duration::from_millis(150);
const DEFAULT_WIDTH: f32 = 244.;

/// What the feature asks the host to do; paths are task-relative (`.context/plans/x.md`).
pub(crate) enum ArtifactsEvent {
    OpenEditor(PathBuf),
    AttachToChat(PathBuf),
    /// The id of the chat whose plan this file copies.
    ShowPlanInChat(String),
    /// The list was dragged to this width.
    Width(f32),
    /// The pinned paths, newest pin first, for the host to save.
    Pins(Vec<String>),
}

pub(crate) struct Artifacts {
    /// The task root, which holds `.context`.
    dir: PathBuf,
    focus: FocusHandle,
    /// Arranged: see `list::arrange`.
    items: Vec<Item>,
    rows: Vec<Row>,
    /// Path inside `.context`. Kept after the file vanishes, see `removed`.
    selected: Option<String>,
    /// What the preview was last told to show.
    previewed: Option<String>,
    /// The selected file left `.context`; the preview keeps it until the selection moves.
    removed: bool,
    /// The file beside the one that left, where the selection goes next.
    beside: Option<String>,
    filter: String,
    filter_input: Entity<InputState>,
    folded: HashSet<String>,
    /// Pinned paths inside `.context`, newest first; may name files that are gone.
    pins: Vec<String>,
    /// The modification time each file had when last previewed; in memory only.
    seen: HashMap<String, SystemTime>,
    /// The first scan marked what was already there as seen.
    baselined: bool,
    /// The task's chats as (id, title), from the host.
    chats: Vec<(String, String)>,
    preview: Entity<Preview>,
    lru: Entity<Lru>,
    resize: Entity<ResizableState>,
    width: f32,
    scroll: UniformListScrollHandle,
    generation: u64,
    loaded: bool,
    started: Instant,
    /// The tab is on screen; set by the host. Only then does the list poll.
    visible: bool,
    error: Option<String>,
    menu: menu::Held,
    _subs: Vec<Subscription>,
    _poll: Task<()>,
}

impl EventEmitter<ArtifactsEvent> for Artifacts {}

#[cfg(test)]
impl Artifacts {
    pub(crate) fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    pub(crate) fn preview(&self) -> &Entity<Preview> {
        &self.preview
    }
}

/// Shows `.context` in Finder, creating it first so an empty task never fails.
pub(crate) fn reveal(task_dir: &Path, window: &mut Window, cx: &mut App) {
    match ensure_context(task_dir) {
        Ok(context) => cx.reveal_path(&context),
        Err(e) => notify_err(window, cx, format!("Couldn't open .context: {e}")),
    }
}

/// `.context` of the task at `task_dir`, created if missing.
pub(crate) fn ensure_context(task_dir: &Path) -> std::io::Result<PathBuf> {
    let context = task_dir.join(".context");
    std::fs::create_dir_all(&context)?;
    Ok(context)
}

/// The files under `root` (`.context`), arranged. Blocking: background only. A missing folder
/// is an empty task, not an error. Dotted names are pruned, not walked.
fn scan(root: &Path) -> Result<Vec<Item>, String> {
    if let Err(e) = std::fs::read_dir(root) {
        return match e.kind() {
            std::io::ErrorKind::NotFound => Ok(vec![]),
            _ => Err(e.to_string()),
        };
    }
    let now = SystemTime::now();
    let mut items = vec![];
    let walk = ignore::WalkBuilder::new(root)
        .hidden(false)
        .standard_filters(false)
        .filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'))
        .build();
    for entry in walk.flatten() {
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if now.duration_since(modified).is_ok_and(|d| d < SETTLE) {
            continue;
        }
        items.push(Item::new(
            rel.to_string_lossy().into_owned(),
            meta.len(),
            modified,
        ));
    }
    arrange(&mut items);
    Ok(items)
}

/// Moves a file to the Trash. Tests delete it, so they never touch the real Trash.
#[cfg(not(test))]
fn trash(path: &Path) -> Result<(), String> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, None)
        .map_err(|e| e.localizedDescription().to_string())
}

#[cfg(test)]
fn trash(path: &Path) -> Result<(), String> {
    std::fs::remove_file(path).map_err(|e| e.to_string())
}

impl Artifacts {
    pub(crate) fn new(
        dir: PathBuf,
        width: Option<f32>,
        pins: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let lru = Lru::new(cx);
        let preview = cx.new(|_| Preview::new(lru.clone()));
        let filter_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter artifacts…"));
        let resize = cx.new(|_| ResizableState::default());
        let subs = vec![
            cx.subscribe_in(
                &preview,
                window,
                |s, _, ev: &PreviewEvent, w, cx| match ev {
                    PreviewEvent::Attach => s.attach(cx),
                    PreviewEvent::Open => s.open(cx),
                    PreviewEvent::ShowInChat => s.show_in_chat(cx),
                    PreviewEvent::TogglePin => s.toggle_pin(cx),
                    PreviewEvent::More(at) => {
                        if let Some(rel) = s.selected.clone() {
                            s.show_menu(rel, *at, w, cx);
                        }
                    }
                },
            ),
            cx.observe(&preview, |_, _, cx| cx.notify()),
            cx.subscribe(&resize, |_, r, _: &ResizablePanelEvent, cx| {
                if let Some(w) = r.read(cx).sizes().get(1) {
                    cx.emit(ArtifactsEvent::Width(w.as_f32()));
                }
            }),
            cx.subscribe_in(
                &filter_input,
                window,
                |s, input, ev: &InputEvent, w, cx| match ev {
                    InputEvent::Change => {
                        s.filter = input.read(cx).value().to_string();
                        s.rebuild();
                        s.keep_selection_shown(cx);
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => s.focus(w, cx),
                    _ => {}
                },
            ),
        ];
        // While the pane is on screen, notice what agents write.
        let poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                let tick = this.update_in(cx, |s, w, cx| {
                    if s.visible {
                        s.refresh(w, cx);
                    }
                });
                if tick.is_err() {
                    break;
                }
            }
        });
        Self {
            dir,
            focus: cx.focus_handle(),
            items: vec![],
            rows: vec![],
            selected: None,
            previewed: None,
            removed: false,
            beside: None,
            filter: String::new(),
            filter_input,
            folded: HashSet::new(),
            pins,
            seen: HashMap::new(),
            baselined: false,
            chats: vec![],
            preview,
            lru,
            resize,
            width: width.unwrap_or(DEFAULT_WIDTH),
            scroll: UniformListScrollHandle::new(),
            generation: 0,
            loaded: false,
            started: Instant::now(),
            visible: false,
            error: None,
            menu: None,
            _subs: subs,
            _poll: poll,
        }
    }

    /// The host shows or hides the tab: shown reads now, hidden stops any video.
    pub(crate) fn set_visible(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.visible == on {
            return;
        }
        self.visible = on;
        match on {
            true => self.refresh(window, cx),
            false => self.preview.update(cx, |p, cx| p.stop(cx)),
        }
    }

    pub(crate) fn visible(&self) -> bool {
        self.visible
    }

    pub(crate) fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
    }

    pub(crate) fn has_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
    }

    /// The width the list takes on the right, for clearing toasts; 0 while it is hidden.
    pub(crate) fn list_width(&self, cx: &App) -> Pixels {
        if !self.loaded || self.items.is_empty() || self.error.is_some() {
            return px(0.);
        }
        (self.resize.read(cx).sizes().get(1).copied()).unwrap_or(px(self.width))
    }

    /// The task's chats, so plan copies can name theirs.
    pub(crate) fn set_chats(&mut self, chats: Vec<(String, String)>, cx: &mut Context<Self>) {
        if self.chats != chats {
            self.chats = chats;
            self.sync_plan(cx);
            cx.notify();
        }
    }

    /// The (id, title) of the chat whose approved plan `rel` copies.
    fn plan_chat(&self, rel: &str) -> Option<&(String, String)> {
        let name = plan_name(rel)?;
        (self.chats.iter()).find(|(_, title)| crate::complete::file_name(title) == name)
    }

    fn sync_plan(&mut self, cx: &mut Context<Self>) {
        let chat = (self.selected.as_ref()).and_then(|rel| self.plan_chat(rel).cloned());
        self.preview.update(cx, |p, cx| p.set_plan_chat(chat, cx));
    }

    fn show_in_chat(&mut self, cx: &mut Context<Self>) {
        let id = (self.selected.as_ref()).and_then(|rel| self.plan_chat(rel).map(|c| c.0.clone()));
        if let Some(id) = id {
            cx.emit(ArtifactsEvent::ShowPlanInChat(id));
        }
    }

    fn sync_pin(&mut self, cx: &mut Context<Self>) {
        let pinned = (self.selected.as_ref()).is_some_and(|rel| self.pins.contains(rel));
        self.preview.update(cx, |p, cx| p.set_pinned(pinned, cx));
    }

    /// Pins or unpins the selected file; the selection stays on it.
    fn toggle_pin(&mut self, cx: &mut Context<Self>) {
        // A removed file has no row, so its pin could never be undone.
        let Some(rel) = self.selected.clone().filter(|_| !self.removed) else {
            return;
        };
        match self.pins.iter().position(|p| *p == rel) {
            Some(at) => drop(self.pins.remove(at)),
            None => self.pins.insert(0, rel.clone()),
        }
        cx.emit(ArtifactsEvent::Pins(self.pins.clone()));
        self.rebuild();
        self.sync_pin(cx);
        self.select(rel, cx);
    }

    fn mark_seen(&mut self, rel: &str) {
        if let Some(i) = self.items.iter().find(|i| i.rel == rel) {
            self.seen.insert(rel.to_string(), i.modified);
        }
    }

    fn unseen(&self, item: &Item) -> bool {
        is_unseen(&self.seen, &item.rel, item.modified)
    }

    /// Files created or changed since they were last previewed, for the rail badge.
    pub(crate) fn unseen_count(&self) -> usize {
        self.items.iter().filter(|i| self.unseen(i)).count()
    }

    fn abs(&self, rel: &str) -> PathBuf {
        self.dir.join(".context").join(rel)
    }

    fn rebuild(&mut self) {
        self.rows = rows(&self.items, &self.filter, &self.folded, &self.pins);
    }

    /// Reads `.context` again, keeping the selection on the same path. Results of a scan that a
    /// newer one overtook are dropped.
    pub(crate) fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        let root = self.dir.join(".context");
        if !self.loaded {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(LOADING_AFTER).await;
                this.update(cx, |_, cx| cx.notify()).ok();
            })
            .detach();
        }
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { scan(&root) })
                .await;
            this.update_in(cx, |s, window, cx| {
                if s.generation == generation {
                    s.apply(result, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn apply(
        &mut self,
        result: Result<Vec<Item>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let first = !std::mem::replace(&mut self.loaded, true);
        let mut new = match result {
            Ok(new) => new,
            Err(e) => {
                if first || self.error.as_ref() != Some(&e) {
                    self.error = Some(e);
                    cx.notify();
                }
                return;
            }
        };
        // A file the scan left for being too fresh is still there.
        let scanned: HashSet<&str> = new.iter().map(|i| i.rel.as_str()).collect();
        let kept: Vec<Item> = (self.items.iter())
            .filter(|old| !scanned.contains(old.rel.as_str()) && self.abs(&old.rel).exists())
            .cloned()
            .collect();
        new.extend(kept);
        arrange(&mut new);
        let mut changed = first || self.error.take().is_some() || new != self.items;
        let before = (self.selected.clone(), self.removed);
        let old = std::mem::replace(&mut self.items, new);
        match self.selected.clone() {
            _ if self.items.is_empty() => {
                self.selected = None;
                self.removed = false;
            }
            Some(s) if self.items.iter().any(|i| i.rel == s) => {
                if self.removed {
                    // It came back: show it afresh.
                    self.removed = false;
                    self.previewed = None;
                }
            }
            Some(s) => {
                if !self.removed {
                    self.removed = true;
                    self.beside = neighbor(&old, &self.items, &s);
                    self.preview.update(cx, |p, cx| p.mark_removed(cx));
                }
            }
            None => {
                // The newest file the filter shows.
                self.rebuild();
                let newest = (self.shown().into_iter()).max_by_key(|&i| self.items[i].modified);
                self.selected = newest.map(|i| self.items[i].rel.clone());
            }
        }
        if !self.baselined {
            // What is there when the pane first scans is not news.
            self.baselined = true;
            self.seen = (self.items.iter())
                .map(|i| (i.rel.clone(), i.modified))
                .collect();
        }
        if let (Some(rel), false) = (self.selected.clone(), self.removed) {
            // The previewed file reloads in place, so its change is seen.
            if self.previewed.as_ref() == Some(&rel) {
                self.mark_seen(&rel);
            }
        }
        changed |= before != (self.selected.clone(), self.removed);
        if changed {
            self.rebuild();
            self.sync_plan(cx);
            cx.notify();
        }
        self.preview
            .update(cx, |p, cx| p.reload_if_changed(window, cx));
    }

    /// Tells the preview what is selected, once it has changed.
    fn sync_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.previewed == self.selected {
            return;
        }
        self.previewed = self.selected.clone();
        if let Some(rel) = self.selected.clone() {
            self.mark_seen(&rel);
        }
        self.sync_plan(cx);
        self.sync_pin(cx);
        let file = (self.selected.as_ref()).map(|rel| (rel.clone(), self.abs(rel)));
        self.preview.update(cx, |p, cx| p.show(file, window, cx));
    }

    fn select(&mut self, rel: String, cx: &mut Context<Self>) {
        self.mark_seen(&rel);
        self.selected = Some(rel.clone());
        self.removed = false;
        let row = self
            .rows
            .iter()
            .position(|r| matches!(r, Row::Item(i) if self.items[*i].rel == rel));
        if let Some(row) = row {
            self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    /// Indexes into `items` of the files in the rows shown.
    fn shown(&self) -> Vec<usize> {
        (self.rows.iter())
            .filter_map(|r| match r {
                Row::Item(i) => Some(*i),
                _ => None,
            })
            .collect()
    }

    /// After the filter changed: a selection it hides moves to the first file shown, or goes.
    fn keep_selection_shown(&mut self, cx: &mut Context<Self>) {
        let shown = self.shown();
        let visible = (self.selected.as_ref())
            .is_some_and(|s| shown.iter().any(|&i| &self.items[i].rel == s));
        if visible {
            return;
        }
        match shown.first() {
            Some(&i) => self.select(self.items[i].rel.clone(), cx),
            None => self.selected = None,
        }
    }

    /// Moves the selection through the rows shown.
    fn go(&mut self, nav: Nav, cx: &mut Context<Self>) {
        let shown = self.shown();
        if shown.is_empty() {
            return;
        }
        let at = (self.selected.as_ref())
            .and_then(|s| shown.iter().position(|&i| &self.items[i].rel == s));
        let to = match (at, nav) {
            (Some(at), _) => nav.to(at, shown.len(), 10),
            (None, Nav::First | Nav::Last) => nav.to(0, shown.len(), 10),
            // A vanished or hidden selection: the neighbor, or the top.
            (None, _) => (self.beside.as_ref().filter(|_| self.removed))
                .and_then(|b| shown.iter().position(|&i| &self.items[i].rel == b))
                .unwrap_or(0),
        };
        self.select(self.items[shown[to]].rel.clone(), cx);
    }

    fn selected_item(&self) -> Option<&Item> {
        let rel = self.selected.as_ref()?;
        self.items.iter().find(|i| &i.rel == rel)
    }

    /// `.context/<rel>`, which the host resolves in the task.
    fn task_path(rel: &str) -> PathBuf {
        Path::new(".context").join(rel)
    }

    fn open(&mut self, cx: &mut Context<Self>) {
        if let Some(i) = self.selected_item() {
            cx.emit(ArtifactsEvent::OpenEditor(Self::task_path(&i.rel)));
        }
    }

    fn attach(&mut self, cx: &mut Context<Self>) {
        if let Some(i) = self.selected_item() {
            cx.emit(ArtifactsEvent::AttachToChat(Self::task_path(&i.rel)));
        }
    }

    fn copy_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(i) = self.selected_item() {
            copy(self.abs(&i.rel).display().to_string(), window, cx);
        }
    }

    fn ask_delete(&mut self, rel: String, window: &mut Window, cx: &mut Context<Self>) {
        let (title, body) = delete_prompt(&rel);
        confirm(
            title,
            body,
            "Delete",
            move |s: &mut Self, w, cx| s.delete(&rel, w, cx),
            window,
            cx,
        );
    }

    /// Trashes the file if it is still there and selects the one beside it. A file already gone
    /// is for the next scan to drop.
    fn delete(&mut self, rel: &str, window: &mut Window, cx: &mut Context<Self>) {
        let abs = self.abs(rel);
        if !abs.exists() {
            return self.refresh(window, cx);
        }
        if let Err(e) = trash(&abs) {
            return notify_err(window, cx, format!("Couldn't delete {rel}: {e}"));
        }
        let old = self.items.clone();
        self.items.retain(|i| i.rel != rel);
        if self.pins.iter().any(|p| p == rel) {
            self.pins.retain(|p| p != rel);
            cx.emit(ArtifactsEvent::Pins(self.pins.clone()));
        }
        if self.selected.as_deref() == Some(rel) {
            self.selected = neighbor(&old, &self.items, rel);
            self.removed = false;
        }
        self.rebuild();
        cx.notify();
    }

    fn show_menu(
        &mut self,
        rel: String,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let abs = self.abs(&rel);
        let this = cx.entity().downgrade();
        let act = |f: fn(&mut Self, &mut Context<Self>)| {
            let this = this.clone();
            let rel = rel.clone();
            move |_: &mut Window, cx: &mut App| {
                this.update(cx, |s, cx| {
                    s.select(rel.clone(), cx);
                    f(s, cx)
                })
                .ok();
            }
        };
        let mut entries = vec![
            menu::item(IconName::SquarePen, "Open in editor", act(Self::open)),
            menu::item(IconName::Paperclip, "Attach to chat", act(Self::attach)),
            match self.pins.contains(&rel) {
                true => menu::item(IconName::PinOff, "Unpin", act(Self::toggle_pin)),
                false => menu::item(IconName::Pin, "Pin", act(Self::toggle_pin)),
            },
            Entry::Sep,
            reveal_entry(&abs),
        ];
        entries.extend(open_in_entries(&abs, |app, a| move |_, _| app.open(&a)));
        entries.push(copy_path_entry(&abs));
        entries.push(copy_relative_entry(
            Self::task_path(&rel).display().to_string(),
        ));
        if self.plan_chat(&rel).is_some() {
            entries.push(menu::item(
                IconName::MessageSquare,
                "Show in chat",
                act(Self::show_in_chat),
            ));
        }
        entries.push(Entry::Sep);
        let (this, name) = (this.clone(), rel.clone());
        entries.push(menu::item(IconName::Trash, "Delete…", move |w, cx| {
            this.update(cx, |s, cx| s.ask_delete(name.clone(), w, cx))
                .ok();
        }));
        let m = menu::open(entries, at, window, cx);
        menu::hold(self, m, |s| &mut s.menu, cx);
    }
}

impl Render for Artifacts {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.previewed != self.selected {
            cx.defer_in(window, |s, w, cx| s.sync_preview(w, cx));
        }
        let theme = cx.theme();
        let dir = self.dir.clone();
        let centered = |child: AnyElement| {
            (v_flex().size_full().items_center().justify_center())
                .child(child)
                .into_any_element()
        };
        let body = if let Some(e) = self.error.clone() {
            centered(empty_state(
                theme,
                "artifacts-error",
                IconName::TriangleAlert,
                "Could not read .context",
                format!("{e}\nPress r to try again"),
            ))
        } else if !self.loaded {
            match self.started.elapsed() >= LOADING_AFTER {
                true => centered(empty_state(
                    theme,
                    "artifacts-loading",
                    IconName::FolderDot,
                    "Loading artifacts…",
                    "",
                )),
                false => div().into_any_element(),
            }
        } else if self.items.is_empty() {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .child(empty_state(
                    theme,
                    "artifacts-empty",
                    IconName::FolderDot,
                    "No artifacts yet",
                    "Plans, notes and screenshots that agents and you add to this task appear here. Stored in .context, deleted with the task, never committed.",
                ))
                .child(
                    Button::new("artifacts-reveal")
                        .label("Reveal in Finder")
                        .on_click(move |_, window, cx| reveal(&dir, window, cx)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(faint(theme))
                        .child("Paste or drop a file into a chat to add one."),
                )
                .into_any_element()
        } else {
            let width = px(self.width);
            h_resizable("artifacts-panel")
                .with_state(&self.resize)
                .with_handle_appearance(quiet_edge())
                .child(
                    resizable_panel()
                        .min_w_0()
                        .child(div().size_full().child(self.preview.clone())),
                )
                .child(
                    resizable_panel()
                        .size(width)
                        .size_range(px(200.)..px(560.))
                        .fixed()
                        .child(self.render_list(window, cx)),
                )
                .into_any_element()
        };
        let root = v_flex()
            .id("artifacts-feature")
            .key_context("ArtifactsFeature")
            .track_focus(&self.focus)
            .size_full()
            .min_w_0()
            .on_action(cx.listener(|s, _: &Confirm, _, cx| s.open(cx)))
            .on_action(cx.listener(|s, _: &AttachArtifact, _, cx| s.attach(cx)))
            .on_action(cx.listener(|s, _: &PinArtifact, _, cx| s.toggle_pin(cx)))
            .on_action(cx.listener(|s, _: &RefreshArtifacts, w, cx| s.refresh(w, cx)))
            .on_action(cx.listener(|s, _: &CopyArtifactPath, w, cx| s.copy_path(w, cx)))
            .on_action(cx.listener(|s, _: &DeleteArtifact, w, cx| {
                if let Some(rel) = s.selected_item().map(|i| i.rel.clone()) {
                    s.ask_delete(rel, w, cx);
                }
            }))
            .on_action(cx.listener(|s, _: &FilterArtifacts, w, cx| {
                s.filter_input.read(cx).focus_handle(cx).focus(w, cx)
            }));
        nav_actions(root, true, cx, Self::go)
            .child(body)
            .children(self.menu.as_ref().map(|(m, _)| menu::layer(m.clone())))
    }
}

/// The file in `new` beside `rel`'s place in `old`: the one after it, else the one before.
fn neighbor(old: &[Item], new: &[Item], rel: &str) -> Option<String> {
    let at = old.iter().position(|i| i.rel == rel)?;
    let live = |i: &&Item| new.iter().any(|n| n.rel == i.rel);
    (old[at + 1..].iter().find(live))
        .or_else(|| old[..at].iter().rev().find(live))
        .map(|i| i.rel.clone())
}

/// The delete dialog's title and body.
fn delete_prompt(rel: &str) -> (String, &'static str) {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    (
        format!("Delete {name}?"),
        "Removes it from .context. Chats that mention it won't find it anymore.",
    )
}

/// `plans/<name> plan.md` is the copy of an approved plan titled `<name>`.
fn plan_name(rel: &str) -> Option<&str> {
    rel.strip_prefix("plans/")?.strip_suffix(" plan.md")
}

fn is_unseen(seen: &HashMap<String, SystemTime>, rel: &str, modified: SystemTime) -> bool {
    seen.get(rel) != Some(&modified)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn plan_copies_are_plans_dir_files_ending_in_plan() {
        assert_eq!(plan_name("plans/Fix login plan.md"), Some("Fix login"));
        assert_eq!(plan_name("plans/notes.md"), None);
        assert_eq!(plan_name("scratch/x plan.md"), None);
    }

    #[::core::prelude::v1::test]
    fn delete_prompt_names_the_file() {
        let (title, body) = delete_prompt("attachments/b.png");
        assert_eq!(title, "Delete b.png?");
        assert_eq!(
            body,
            "Removes it from .context. Chats that mention it won't find it anymore."
        );
    }

    #[::core::prelude::v1::test]
    fn neighbor_prefers_the_next_file_then_the_previous() {
        let item = |rel: &str| Item::new(rel.into(), 1, SystemTime::UNIX_EPOCH);
        let old = [item("a"), item("b"), item("c")];
        let without = |gone: &[&str]| -> Vec<Item> {
            (old.iter().filter(|i| !gone.contains(&i.rel.as_str())))
                .cloned()
                .collect()
        };
        assert_eq!(neighbor(&old, &without(&["b"]), "b").as_deref(), Some("c"));
        assert_eq!(neighbor(&old, &without(&["c"]), "c").as_deref(), Some("b"));
        assert_eq!(
            neighbor(&old, &without(&["b", "c"]), "b").as_deref(),
            Some("a")
        );
        assert_eq!(neighbor(&old, &[], "b"), None);
    }

    fn put(root: &Path, rel: &str, age: u64) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "x").unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(age))
            .unwrap();
    }

    #[::core::prelude::v1::test]
    fn scan_skips_dotted_paths_and_unsettled_files() {
        let root = std::env::temp_dir().join(format!("shark-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert!(scan(&root).unwrap().is_empty(), "a missing folder is empty");
        put(&root, "plans/a.md", 10);
        put(&root, ".hidden.md", 10);
        put(&root, ".git/config", 10);
        put(&root, "scratch/.cache/x.bin", 10);
        put(&root, "scratch/fresh.txt", 0);
        let rels: Vec<String> = scan(&root).unwrap().into_iter().map(|i| i.rel).collect();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(rels, ["plans/a.md"]);
    }

    #[::core::prelude::v1::test]
    fn unseen_until_seen_at_the_same_mtime() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
        let mut seen = HashMap::new();
        assert!(is_unseen(&seen, "a.md", t));
        seen.insert("a.md".to_string(), t);
        assert!(!is_unseen(&seen, "a.md", t));
        assert!(is_unseen(&seen, "a.md", t + Duration::from_secs(1)));
    }
}
