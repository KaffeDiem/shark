//! One agent conversation (a chat tab): transcript blocks, composer, and the claude process.
mod completion;
mod side;

use crate::agent::{self, Agent, Ev};
use crate::complete::{self, Item};
use crate::state::{ChatMeta, Workspace};
use crate::*;
use completion::Menu;
use gpui_kit::assets::IconName;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{
    Enter, Escape, IndentInline, InputContent, InputEvent, MoveDown, MoveUp, Textarea,
    TextareaState,
};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;
use side::Side;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Instant;

struct Call {
    id: String,
    name: String,
    summary: String,
    /// The full input, one `key: value` per line and capped, for the row's tooltip.
    input: String,
    result: Option<SharedString>,
    is_error: bool,
    open: bool,
}

enum Block {
    /// `true` while queued: sent mid-turn, and claude hasn't read it yet.
    User(String, bool),
    Text {
        md: Entity<TextViewState>,
        done: bool,
    },
    /// Consecutive tool calls between two pieces of prose, shown as one summary row.
    Tools {
        calls: Vec<Call>,
        open: bool,
    },
    /// Skills and subagents stay visible on their own row: they say what the agent is doing.
    Tool(Call),
    Info(String),
}

/// A transcript row: a block, or one call inside an expanded tool group.
type Row = (usize, Option<usize>);

fn standalone(name: &str) -> Option<IconName> {
    match name {
        "Skill" => Some(IconName::BookOpen),
        "Agent" | "Task" => Some(IconName::Bot),
        _ => None,
    }
}

/// "Read 3 files, ran 2 commands" for a tool group, in first-use order.
fn group_summary(calls: &[Call]) -> String {
    let mut parts: Vec<(&str, &str, &str, usize)> = vec![];
    for c in calls {
        let (verb, one, many) = match c.name.as_str() {
            "Read" => ("read", "file", "files"),
            "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => ("edited", "file", "files"),
            "Bash" => ("ran", "command", "commands"),
            "Grep" | "Glob" => ("searched", "pattern", "patterns"),
            "WebSearch" | "WebFetch" => ("looked up", "page", "pages"),
            _ => ("used", "tool", "tools"),
        };
        match parts.iter_mut().find(|p| p.0 == verb) {
            Some(p) => p.3 += 1,
            None => parts.push((verb, one, many, 1)),
        }
    }
    let s = parts
        .iter()
        .map(|&(v, one, many, n)| format!("{v} {n} {}", if n == 1 { one } else { many }))
        .collect::<Vec<_>>()
        .join(", ");
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// A message typed while the agent runs. The top one goes out when a turn ends well; any can
/// go out now to steer the running turn.
struct Queued {
    /// With its chips, so editing puts them back.
    content: InputContent,
    /// First line, capped, so a long paste isn't shaped on every frame.
    preview: SharedString,
}

impl Queued {
    fn new(content: InputContent) -> Self {
        let first = content.text().trim().lines().next().unwrap_or_default();
        let mut preview: String = first.chars().take(200).collect();
        if preview.len() < content.text().trim().len() {
            preview.push('…');
        }
        Queued {
            content,
            preview: preview.into(),
        }
    }

    fn text(&self) -> String {
        self.content.text().trim().to_string()
    }
}

pub enum ChatEvent {
    /// `meta` changed (session, model or title); the app saves it.
    Meta,
    TurnDone,
}

pub struct Chat {
    pub ws: Workspace,
    pub meta: ChatMeta,
    blocks: Vec<Block>,
    /// What the list shows, derived from `blocks`; see `sync_rows`.
    rows: Vec<Row>,
    sel: usize,
    pub busy: bool,
    /// Background tasks (subagents, shells) still running, by description; the turn can end
    /// before they do.
    pub background: Vec<String>,
    /// When the running turn started.
    pub started: Option<Instant>,
    /// Set while a git step runs in these worktrees; sends wait for it.
    pub hold: bool,
    /// Set while the workspace's worktrees are being removed; sends are refused.
    pub archiving: bool,
    /// Told to the agent with the next message (a repo joined the workspace mid-session).
    note: Option<String>,
    agent: Option<Agent>,
    pub(crate) composer: Entity<TextareaState>,
    /// Model the running process was started with; a mismatch respawns it on the next send.
    agent_model: Option<String>,
    /// Virtualized: only visible rows render, which matters while a spinner redraws every frame.
    list: ListState,
    pub focus: FocusHandle,
    reader: Option<Task<()>>,
    files: Vec<Item>,
    commands: Vec<Item>,
    menu: Option<Menu>,
    menu_scroll: ScrollHandle,
    indexing: Option<Task<()>>,
    probing: Option<Task<()>>,
    /// User blocks claude hasn't echoed yet, oldest first.
    unread: VecDeque<usize>,
    /// The turn was interrupted; its result reads "interrupted".
    cut: bool,
    /// SIGTERM fallback for an interrupt claude doesn't act on.
    stopping: Option<Task<()>>,
    /// Applying stored history: nothing is emitted to the app.
    replaying: bool,
    queue: Vec<Queued>,
    qsel: usize,
    qfocus: FocusHandle,
    qscroll: ScrollHandle,
    /// The queue slot of the message being edited in the composer. The queue waits meanwhile,
    /// so the message doesn't lose its turn.
    editing: Option<usize>,
    /// A message was sent into a running turn and claude may not have read it yet.
    pub steered: bool,
    /// What the CLI reported at init: its model, permission mode and MCP servers.
    reported: Option<String>,
    side: Option<Side>,
    _subs: Vec<Subscription>,
}

/// How long an interrupt may take before the process is stopped instead.
const INTERRUPT_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// `default` means "no --model flag".
pub(crate) const MODELS: &[&str] = &["default", "fable", "opus", "sonnet", "haiku"];

impl EventEmitter<ChatEvent> for Chat {}

fn summarize(input: &Value, root: &str) -> String {
    let field = [
        "skill",
        "command",
        "file_path",
        "pattern",
        "path",
        "url",
        "description",
        "prompt",
    ]
    .iter()
    .find_map(|k| input[k].as_str());
    let s = field
        .map(str::to_string)
        .unwrap_or_else(|| input.to_string());
    let s = s.lines().next().unwrap_or_default();
    // Paths inside the workspace read better as repo/file.
    let s = s
        .strip_prefix(root)
        .map_or(s, |r| r.trim_start_matches('/'));
    s.chars().take(120).collect()
}

/// A tab title from a message: its first line, cut at a word near 40 characters.
fn title_of(text: &str) -> String {
    let line = text.trim().lines().next().unwrap_or_default();
    if line.chars().count() <= 40 {
        return line.to_string();
    }
    let cut: String = line.chars().take(40).collect();
    let cut = cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head);
    format!("{}…", cut.trim_end())
}

/// The first `n` chars, with "…" when cut.
fn cap(s: &str, n: usize) -> String {
    let mut c: String = s.chars().take(n).collect();
    if c.len() < s.len() {
        c.push('…');
    }
    c
}

fn full_input(input: &Value) -> String {
    let s = match input.as_object() {
        Some(o) => (o.iter())
            .map(|(k, v)| {
                format!(
                    "{k}: {}",
                    v.as_str().map_or_else(|| v.to_string(), Into::into)
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        None => serde_json::to_string_pretty(input).unwrap_or_default(),
    };
    cap(&s, 2000)
}

/// Keys (space-separated) and what they do, for a hint row.
fn hint(keys: &str, label: &'static str) -> Div {
    h_flex()
        .gap_1()
        .children(
            keys.split(' ')
                .map(|k| Kbd::new(Keystroke::parse(k).unwrap())),
        )
        .child(label)
}

/// "48s", "2m 5s".
pub(crate) fn duration(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        _ => format!("{}m {}s", secs / 60, secs % 60),
    }
}

impl Chat {
    pub fn new(ws: Workspace, meta: ChatMeta, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 10)
                .placeholder("Message the agent (@ to add files, / for skills, Enter to send, Esc for normal mode)")
                .submit_on_enter(true)
        });
        let _sub =
            cx.subscribe_in(
                &composer,
                window,
                |this, _, ev: &InputEvent, window, cx| match ev {
                    InputEvent::PressEnter {
                        shift: false,
                        secondary,
                    } => this.send(*secondary, window, cx),
                    InputEvent::Change => this.update_menu(cx),
                    InputEvent::Blur => this.close_menu(cx),
                    _ => {}
                },
            );
        let ws_name = ws.name.clone();
        let mut chat = Chat {
            ws,
            meta,
            blocks: vec![],
            rows: vec![],
            sel: 0,
            busy: false,
            background: vec![],
            started: None,
            hold: false,
            archiving: false,
            note: None,
            agent: None,
            composer,
            agent_model: None,
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            focus: cx.focus_handle(),
            reader: None,
            commands: complete::cached_commands(&ws_name),
            files: vec![],
            menu: None,
            menu_scroll: ScrollHandle::new(),
            indexing: None,
            probing: None,
            unread: VecDeque::new(),
            cut: false,
            stopping: None,
            replaying: false,
            queue: vec![],
            qsel: 0,
            qfocus: cx.focus_handle(),
            qscroll: ScrollHandle::new(),
            editing: None,
            steered: false,
            side: None,
            reported: None,
            _subs: vec![_sub],
        };
        // Warm both sources now so the first `@` or `/` doesn't wait.
        chat.reindex(cx);
        chat.reprobe(cx);
        chat.list.set_follow_mode(FollowMode::Tail);
        // Read history off the UI thread, up to its current end: a process started meanwhile
        // appends to the same file, and its lines arrive live.
        let path = agent::transcript_path(&chat.meta.id);
        let len = std::fs::metadata(&path).map_or(0, |m| m.len());
        cx.spawn(async move |this, cx| {
            let evs = (cx.background_executor())
                .spawn(async move { agent::read_history(&path, len) })
                .await;
            this.update(cx, |c, cx| c.load_history(evs, cx)).ok();
        })
        .detach();
        chat
    }

    /// Replays history through the same path as live events, ahead of anything that arrived
    /// while it loaded, then lays out the rows once.
    fn load_history(&mut self, evs: Vec<Ev>, cx: &mut Context<Self>) {
        if evs.is_empty() {
            return;
        }
        let live = std::mem::take(&mut self.blocks);
        let unread = std::mem::take(&mut self.unread);
        // state.json's session (it may have been reset), or the live process's
        let kept = (
            self.busy,
            std::mem::take(&mut self.background),
            self.started,
            self.cut,
            self.meta.session_id.clone(),
        );
        (self.busy, self.cut, self.replaying) = (false, false, true);
        for ev in evs {
            // A turn that never got its result was cut off by quitting (or a crash).
            if matches!(ev, Ev::Init { .. }) && self.busy {
                self.blocks.push(Block::Info("interrupted".into()));
            }
            self.apply_inner(ev, cx);
        }
        if self.busy {
            self.blocks.push(Block::Info("interrupted".into()));
        }
        self.interrupt();
        self.replaying = false;
        (
            self.busy,
            self.background,
            self.started,
            self.cut,
            self.meta.session_id,
        ) = kept;
        let n = self.blocks.len();
        self.blocks.extend(live);
        self.unread = unread.into_iter().map(|i| i + n).collect();
        self.rows = self.build_rows();
        self.list.reset(self.rows.len());
        self.list.set_follow_mode(FollowMode::Tail);
        self.sel = self.rows.len().saturating_sub(1);
        if self.meta.title.is_empty()
            && let Some(Block::User(t, _)) =
                self.blocks.iter().find(|b| matches!(b, Block::User(..)))
        {
            self.meta.title = title_of(t);
            cx.emit(ChatEvent::Meta);
        }
        cx.notify();
    }

    /// Whether anything is still going on: a turn, or tasks it left running in the background.
    pub fn working(&self) -> bool {
        self.busy || !self.background.is_empty()
    }

    /// "Read: api/src/a.rs" for the call in flight.
    pub fn running_tool(&self) -> Option<String> {
        let calls = self.blocks.iter().rev().flat_map(|b| match b {
            Block::Tool(c) => std::slice::from_ref(c),
            Block::Tools { calls, .. } => calls.as_slice(),
            _ => &[],
        });
        let c = calls.rev().find(|c| c.result.is_none())?;
        Some(format!("{}: {}", c.name, c.summary))
    }

    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    pub fn model(&self) -> &str {
        self.meta.model.as_deref().unwrap_or(MODELS[0])
    }

    /// The tab label.
    pub fn title(&self) -> SharedString {
        match self.meta.title.is_empty() {
            true => "New chat".into(),
            false => self.meta.title.clone().into(),
        }
    }

    /// Takes effect on the next send, which restarts the process on the new model.
    pub fn set_model(&mut self, m: &str, cx: &mut Context<Self>) {
        let model = (m != MODELS[0]).then(|| m.to_string());
        if self.meta.model != model {
            self.meta.model = model;
            cx.emit(ChatEvent::Meta);
            cx.notify();
        }
    }

    pub fn focus_composer(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |c, cx| c.focus(window, cx));
    }

    /// Re-lists files in the background; the old list serves until it lands.
    fn reindex(&mut self, cx: &mut Context<Self>) {
        if self.indexing.is_some() {
            return;
        }
        let dir = self.ws.dir();
        let repos: Vec<_> = (self.ws.repos.iter())
            .map(|r| (r.name.clone(), dir.join(&r.name)))
            .collect();
        self.indexing = Some(cx.spawn(async move |this, cx| {
            let files = cx
                .background_executor()
                .spawn(async move { complete::files(&repos) })
                .await;
            this.update(cx, |c, cx| {
                c.indexing = None;
                c.files = files;
                c.update_menu(cx);
            })
            .ok();
        }));
    }

    /// Asks claude for its current skills and commands in the background.
    fn reprobe(&mut self, cx: &mut Context<Self>) {
        if self.probing.is_some() || cfg!(test) {
            return;
        }
        let (name, dir) = (self.ws.name.clone(), self.ws.dir());
        self.probing = Some(cx.spawn(async move |this, cx| {
            let cmds = cx
                .background_executor()
                .spawn(async move { complete::probe_commands(&name, &dir) })
                .await;
            this.update(cx, |c, cx| {
                c.probing = None;
                if let Some(cmds) = cmds {
                    c.commands = cmds;
                    c.update_menu(cx);
                }
            })
            .ok();
        }));
    }

    /// Dropped or pasted files: copied into the workspace off the main thread, then mentioned at
    /// the cursor in drop order. Pasted image bytes come as `(bytes, ext)`.
    fn attach(
        &mut self,
        paths: Vec<PathBuf>,
        image: Option<(Vec<u8>, &'static str)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dir = self.ws.dir();
        cx.spawn_in(window, async move |this, cx| {
            let done = cx
                .background_executor()
                .spawn(async move {
                    let mut out: Vec<_> = paths.iter().map(|p| complete::attach(&dir, p)).collect();
                    out.extend(image.map(|(b, ext)| {
                        complete::attach_bytes(&dir, &format!("Pasted image.{ext}"), &b)
                    }));
                    out
                })
                .await;
            this.update_in(cx, |c, window, cx| {
                // The user may have selected text while it copied; insert beside it, not over it.
                c.composer.update(cx, |c, cx| {
                    let at = c.cursor();
                    c.set_selected_range(at..at, cx)
                });
                for r in done {
                    match r {
                        Ok(p) => {
                            let label = Path::new(&p).file_name().unwrap_or_default();
                            let label = label.to_string_lossy().into_owned();
                            c.insert_mention(complete::mention(&p), label, window, cx);
                        }
                        Err(e) => window.push_notification(Notification::error(e), cx),
                    }
                }
                c.focus_composer(window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// cmd-v of Finder files or image data attaches them; anything else pastes as text.
    fn paste(&mut self, item: &ClipboardItem, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let entries = item.entries();
        if let Some(p) = entries.iter().find_map(|e| match e {
            ClipboardEntry::ExternalPaths(p) => Some(p.paths().to_vec()),
            _ => None,
        }) {
            self.attach(p, None, window, cx);
            return true;
        }
        // Text wins over a rendered image of it, as copied from a word processor.
        if entries
            .iter()
            .any(|e| matches!(e, ClipboardEntry::String(_)))
        {
            return false;
        }
        let Some(img) = entries.iter().find_map(|e| match e {
            ClipboardEntry::Image(i) => Some(i),
            _ => None,
        }) else {
            return false;
        };
        self.attach(
            vec![],
            Some((img.bytes.clone(), img.format.extension())),
            window,
            cx,
        );
        true
    }

    /// Enter: sends, or queues while a turn or git step runs. `now` (cmd-enter) steers the
    /// running turn instead; claude reads it at its next tool call.
    fn send(&mut self, now: bool, window: &mut Window, cx: &mut Context<Self>) {
        let content = self.composer.read(cx).content();
        let text = content.text().trim().to_string();
        if text.is_empty() {
            // Clearing an edited message and pressing Enter deletes it.
            if self.editing.take().is_some() {
                self.drain(cx);
                cx.notify();
            }
            return;
        }
        let busy = self.busy;
        let sent = !self.hold && (!busy || now) && self.send_text(text, now, cx);
        self.steered |= sent && busy;
        if !sent && (self.busy || self.hold) {
            let at = self.queue_slot();
            self.queue.insert(at, Queued::new(content));
            self.qsel = at;
        } else if !sent {
            return; // spawn or write failed; the error is in the transcript, keep the draft
        }
        self.editing = None;
        self.composer
            .update(cx, |c, cx| c.set_value("", window, cx));
        if sent {
            self.drain(cx); // an edit that went straight out lets the rest follow
        }
        cx.notify();
    }

    /// Sends the top queued message if the agent is free.
    pub fn drain(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.hold || self.editing.is_some() || self.queue.is_empty() {
            return;
        }
        let q = self.queue.remove(0); // editing is None here
        match self.send_text(q.text(), false, cx) {
            true => self.qsel = self.qsel.saturating_sub(1),
            false => self.queue.insert(0, q),
        }
        self.after_queue_change(cx);
    }

    /// Where a message goes back into the queue: the slot of the one being edited, else last.
    fn queue_slot(&self) -> usize {
        self.editing
            .unwrap_or(self.queue.len())
            .min(self.queue.len())
    }

    /// Removes a queued message, keeping the slot of the one being edited in place.
    fn take_queued(&mut self, ix: usize) -> Queued {
        if let Some(e) = &mut self.editing
            && *e > ix
        {
            *e -= 1;
        }
        self.queue.remove(ix)
    }

    fn after_queue_change(&mut self, cx: &mut Context<Self>) {
        self.qsel = self.qsel.min(self.queue.len().saturating_sub(1));
        self.qscroll.scroll_to_item(self.qsel);
        cx.notify();
    }

    pub fn focus_queue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.queue.is_empty() {
            return self.focus_composer(window, cx);
        }
        self.qfocus.focus(window, cx);
        self.after_queue_change(cx);
    }

    fn queue_select(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        // The composer sits below the last row, so moving past it lands there.
        if ix >= self.queue.len() {
            return self.focus_composer(window, cx);
        }
        self.qsel = ix;
        self.after_queue_change(cx);
    }

    /// Sends the selected message into the running turn.
    fn steer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.qsel >= self.queue.len() {
            return;
        }
        if self.hold {
            self.push(
                Block::Info("a git step is running; it stays queued".into()),
                cx,
            );
            return;
        }
        let editing = self.editing;
        let q = self.take_queued(self.qsel);
        let busy = self.busy;
        if self.send_text(q.text(), true, cx) {
            // Claude may hold it unread until this turn ends; don't restart the process under it.
            self.steered |= busy;
        } else {
            self.editing = editing;
            self.queue.insert(self.qsel, q);
        }
        self.after_queue_change(cx);
        if self.queue.is_empty() {
            self.focus_composer(window, cx);
        }
    }

    fn delete_queued(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.qsel >= self.queue.len() {
            return;
        }
        self.take_queued(self.qsel);
        self.after_queue_change(cx);
        if self.queue.is_empty() {
            self.focus_composer(window, cx);
        }
    }

    fn move_queued(&mut self, d: isize, cx: &mut Context<Self>) {
        let to = self.qsel as isize + d;
        if to < 0 || to as usize >= self.queue.len() {
            return;
        }
        self.queue.swap(self.qsel, to as usize);
        self.qsel = to as usize;
        self.after_queue_change(cx);
    }

    /// Loads the selected message into the composer; Enter puts it back in the same slot.
    /// A draft already there is queued first, so nothing typed is lost.
    fn edit_queued(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.qsel >= self.queue.len() {
            return;
        }
        let mut ix = self.qsel;
        let item = self.take_queued(ix);
        let draft = self.composer.read(cx).content();
        if !draft.text().trim().is_empty() {
            let at = self.queue_slot();
            self.queue.insert(at, Queued::new(draft));
            if at <= ix {
                ix += 1;
            }
        }
        self.editing = Some(ix);
        let end = item.content.text().len();
        self.composer.update(cx, |c, cx| {
            c.set_value(item.content, window, cx);
            c.set_selected_range(end..end, cx);
            c.focus(window, cx);
        });
        self.after_queue_change(cx);
    }

    /// Sends a host-written prompt (git steps). A draft in the composer rides along as extra
    /// instructions, so "Commit and push" can carry "…and mention the ticket".
    pub fn ask(&mut self, prompt: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let draft = self.composer.read(cx).value().trim().to_string();
        let text = match draft.is_empty() {
            true => prompt.to_string(),
            false => format!("{prompt}\n\nAlso: {draft}"),
        };
        let sent = self.send_text(text, false, cx);
        if sent && !draft.is_empty() {
            self.editing = None;
            self.composer
                .update(cx, |c, cx| c.set_value("", window, cx));
        }
        sent
    }

    /// Returns whether the text went out; while a turn runs only if `steer`. Mid-turn, claude
    /// holds it and reads it at its next step.
    fn send_text(&mut self, text: String, steer: bool, cx: &mut Context<Self>) -> bool {
        if text.is_empty() || (self.busy && !steer) {
            return false;
        }
        if self.hold {
            self.push(
                Block::Info("a git step is running; send again when it finishes".into()),
                cx,
            );
            return false;
        }
        if self.archiving {
            self.push(
                Block::Info("this workspace is being archived; nothing was sent".into()),
                cx,
            );
            return false;
        }
        // Never mid-turn: a steer would kill the turn it means to steer.
        if self.agent.is_some()
            && self.agent_model != self.meta.model
            && !self.busy
            && !self.steered
        {
            self.agent = None; // drop stops it; the respawn resumes the same session on the new model
            self.background.clear(); // its reader is replaced below, so its Exited never arrives
        }
        if self.agent.is_none() {
            match agent::spawn(&self.ws, &self.meta) {
                Ok((a, rx)) => {
                    self.agent = Some(a);
                    self.agent_model = self.meta.model.clone();
                    self.reader = Some(Self::read_events(rx, Self::apply, cx));
                }
                Err(e) => {
                    self.push(Block::Info(format!("failed to start claude: {e}")), cx);
                    return false;
                }
            }
        }
        let wire = match &self.note {
            Some(n) => format!("{n}\n\n{text}"),
            None => text.clone(),
        };
        if let Err(e) = self.agent.as_mut().unwrap().send(&wire) {
            self.push(Block::Info(format!("send failed: {e}")), cx);
            return false;
        }
        self.note = None;
        self.sent(text, cx);
        true
    }

    /// Feeds a process's events to `apply` until it ends or the chat is gone.
    fn read_events(
        rx: gpui_kit::base::async_util::Receiver<Ev>,
        apply: fn(&mut Self, Ev, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn(async move |this, cx| {
            while let Ok(ev) = rx.recv().await {
                if this.update(cx, |c, cx| apply(c, ev, cx)).is_err() {
                    break;
                }
            }
        })
    }

    /// A repo joined the workspace: refresh `@` files, show it, and tell the agent next turn
    /// (its CLAUDE.md was read at session start).
    pub fn repos_changed(&mut self, ws: &Workspace, note: String, cx: &mut Context<Self>) {
        self.ws.repos = ws.repos.clone();
        self.push(Block::Info(note.clone()), cx);
        self.note = Some(match self.note.take() {
            Some(old) => format!("{old}\n{note}"),
            None => note,
        });
        self.reindex(cx);
    }

    fn sent(&mut self, text: String, cx: &mut Context<Self>) {
        let queued = self.busy;
        if !queued {
            self.started = Some(Instant::now());
        }
        self.busy = true;
        if self.meta.title.is_empty() {
            self.meta.title = title_of(&text);
            cx.emit(ChatEvent::Meta);
        }
        // Jump to the end so push follows the new message and the reply.
        self.sel = self.rows.len().saturating_sub(1);
        self.list.set_follow_mode(FollowMode::Tail);
        self.unread.push_back(self.blocks.len());
        self.push(Block::User(text, queued), cx);
    }

    /// Interrupts a running turn and keeps the process warm; SIGTERM if claude doesn't stop
    /// within `INTERRUPT_GRACE` (unless background tasks would die with it), or on a second press. With only background tasks left, stops
    /// the process, since that is the only way to end them. Idle, does nothing.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        let working = self.working();
        let Some(a) = &mut self.agent else {
            return;
        };
        if !self.busy || self.stopping.is_some() {
            if working {
                a.stop();
                self.push(Block::Info("stopping…".into()), cx);
            }
            return;
        }
        if a.interrupt().is_err() {
            a.stop();
        }
        self.stopping = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(INTERRUPT_GRACE).await;
            this.update(cx, |c, cx| {
                if let Some(a) = &c.agent
                    && c.busy
                {
                    // SIGTERM also ends background tasks: with some running, let the user decide.
                    if !c.background.is_empty() {
                        let msg =
                            "not responding; ctrl-c again to stop it and its background tasks";
                        c.push(Block::Info(msg.into()), cx);
                    } else {
                        a.stop();
                        c.push(Block::Info("not responding; stopping…".into()), cx);
                    }
                }
            })
            .ok();
        }));
        self.push(Block::Info("interrupting…".into()), cx);
    }

    /// Messages claude never read, once its process is gone.
    fn drop_unread(&mut self) {
        let n = self.unread.len();
        for bi in std::mem::take(&mut self.unread) {
            if let Some(Block::User(_, queued)) = self.blocks.get_mut(bi) {
                *queued = false;
            }
        }
        if n > 0 {
            let s = if n == 1 { "" } else { "s" };
            self.blocks.push(Block::Info(format!(
                "{n} message{s} not delivered; send again"
            )));
        }
    }

    /// The process is gone (or going): forget its state, end its calls, flag what it never read.
    fn drop_process(&mut self) {
        self.agent = None; // drop stops it
        self.background.clear();
        self.stopping = None;
        self.cut = false;
        self.steered = false;
        self.interrupt();
        self.drop_unread();
    }

    /// Forgets the claude session and stops the agent; the next send starts a new conversation.
    pub fn new_conversation(&mut self, cx: &mut Context<Self>) {
        let was_working = self.working();
        self.meta.session_id = None;
        cx.emit(ChatEvent::Meta);
        self.reader = None; // its Exited would be about the old process
        self.busy = false;
        self.follow(cx, |this, _| {
            this.drop_process();
            this.blocks.push(Block::Info("new conversation".into()));
        });
        if was_working {
            cx.emit(ChatEvent::TurnDone);
        }
    }

    /// Calls that never got a result once the process is gone.
    fn interrupt(&mut self) {
        for bi in 0..self.blocks.len() {
            let calls = match &mut self.blocks[bi] {
                Block::Tool(c) => std::slice::from_mut(c),
                Block::Tools { calls, .. } => calls.as_mut_slice(),
                _ => continue,
            };
            let mut touched = false;
            for c in calls.iter_mut().filter(|c| c.result.is_none()) {
                c.result = Some("(interrupted)".into());
                c.is_error = true;
                touched = true;
            }
            if touched {
                self.remeasure(bi); // an open call's output changed height
            }
        }
    }

    fn build_rows(&self) -> Vec<Row> {
        let mut rows = vec![];
        for (ix, b) in self.blocks.iter().enumerate() {
            rows.push((ix, None));
            if let Block::Tools { calls, open: true } = b {
                rows.extend((0..calls.len()).map(|j| (ix, Some(j))));
            }
        }
        rows
    }

    /// Re-derives the rows and tells the list which were added or removed. `touched` is a
    /// block that changed in place: off-screen rows keep their measured height until told.
    fn sync_rows(&mut self, touched: Option<usize>) {
        let rows = self.build_rows();
        let old = std::mem::replace(&mut self.rows, rows);
        let new = &self.rows;
        let pre = old.iter().zip(new).take_while(|(a, b)| a == b).count();
        let suf = (old[pre..].iter().rev())
            .zip(new[pre..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        self.list
            .splice(pre..old.len() - suf, new.len() - pre - suf);
        if let Some(bi) = touched {
            self.remeasure(bi);
        }
    }

    fn remeasure(&self, bi: usize) {
        let start = self.rows.partition_point(|r| r.0 < bi);
        let end = self.rows.partition_point(|r| r.0 <= bi);
        self.list.remeasure_items(start..end);
    }

    /// Keeps the cursor on the newest row if it was already on the last one. The list itself
    /// follows the tail only while scrolled to the bottom, so reading further up stays put.
    fn follow(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Self, &mut Context<Self>)) {
        let at_end = self.sel + 1 >= self.rows.len();
        f(self, cx);
        self.sync_rows(self.blocks.len().checked_sub(1)); // streaming grows the last block
        if at_end {
            self.sel = self.rows.len().saturating_sub(1);
        }
        cx.notify();
    }

    fn push(&mut self, b: Block, cx: &mut Context<Self>) {
        self.follow(cx, |this, _| this.blocks.push(b));
    }

    fn apply(&mut self, ev: Ev, cx: &mut Context<Self>) {
        // Only a turn that ended well sends the next message; after a stop or an error the
        // queue waits for the user.
        let ok = matches!(
            ev,
            Ev::Result {
                is_error: false,
                ..
            }
        );
        self.follow(cx, |this, cx| this.apply_inner(ev, cx));
        if ok {
            self.drain(cx);
        }
    }

    /// The streaming text block a reply continues: the last block, looking past messages
    /// queued while it streams (they'd otherwise split the reply in two).
    fn live_text(&self) -> Option<usize> {
        let bi = (self.blocks.iter()).rposition(|b| !matches!(b, Block::User(_, true)))?;
        matches!(self.blocks[bi], Block::Text { done: false, .. }).then_some(bi)
    }

    fn apply_inner(&mut self, ev: Ev, cx: &mut Context<Self>) {
        match ev {
            Ev::Init {
                session_id: id,
                model,
                permission_mode,
                mcp,
            } => {
                if self.meta.session_id.as_ref() != Some(&id) {
                    self.meta.session_id = Some(id);
                    if !self.replaying {
                        cx.emit(ChatEvent::Meta);
                    }
                }
                let mut r = format!("running {model} · permissions: {permission_mode}");
                if !mcp.is_empty() {
                    r += &format!(" · MCP: {}", mcp.join(", "));
                }
                self.reported = Some(r);
                // Each turn starts with init, including ones the CLI starts itself when a
                // background task finishes.
                if !self.busy {
                    self.started = Some(Instant::now());
                }
                self.busy = true;
                self.steered = false;
            }
            Ev::Background(tasks) => self.background = tasks,
            Ev::Interrupted => self.cut = true,
            // claude's echo: the oldest message it hadn't read is now part of the turn.
            // `ends_with`: a workspace `note` may have been put before the text on the wire.
            Ev::User(text) => match self.unread.front().map(|&bi| &mut self.blocks[bi]) {
                Some(Block::User(t, queued)) if text.ends_with(t.as_str()) => {
                    *queued = false;
                    let bi = self.unread.pop_front().unwrap();
                    self.remeasure(bi);
                }
                _ => self.blocks.push(Block::User(text, false)),
            },
            Ev::TextDelta(s) => match self.live_text() {
                Some(bi) => {
                    if let Block::Text { md, .. } = &self.blocks[bi] {
                        md.update(cx, |m, cx| m.push_str(&s, cx));
                    }
                    self.remeasure(bi);
                }
                None => {
                    let md = cx.new(|cx| TextViewState::markdown(&s, cx));
                    self.blocks.push(Block::Text { md, done: false });
                }
            },
            Ev::Text(full) => match self.live_text() {
                Some(bi) => {
                    if let Block::Text { md, done } = &mut self.blocks[bi] {
                        md.update(cx, |m, cx| m.set_text(&full, cx));
                        *done = true;
                    }
                    self.remeasure(bi);
                }
                None => {
                    let md = cx.new(|cx| TextViewState::markdown(&full, cx));
                    self.blocks.push(Block::Text { md, done: true });
                }
            },
            Ev::ToolUse { id, name, input } => {
                let call = Call {
                    summary: summarize(&input, &self.ws.dir().to_string_lossy()),
                    input: full_input(&input),
                    id,
                    name,
                    result: None,
                    is_error: false,
                    open: false,
                };
                match self.blocks.last_mut() {
                    _ if standalone(&call.name).is_some() => self.blocks.push(Block::Tool(call)),
                    Some(Block::Tools { calls, .. }) => calls.push(call),
                    _ => self.blocks.push(Block::Tools {
                        calls: vec![call],
                        open: false,
                    }),
                }
            }
            Ev::ToolResult { id, text, is_error } => {
                let call = (self.blocks.iter_mut().enumerate().rev()).find_map(|(bi, b)| match b {
                    Block::Tool(c) => (c.id == id).then_some((bi, c)),
                    Block::Tools { calls, .. } => {
                        calls.iter_mut().find(|c| c.id == id).map(|c| (bi, c))
                    }
                    _ => None,
                });
                if let Some((bi, c)) = call {
                    c.result = Some(text.into());
                    c.is_error = is_error;
                    self.remeasure(bi); // an open call shows its output
                }
            }
            Ev::Result {
                is_error,
                text,
                cost,
                secs,
            } => {
                self.busy = false;
                if !self.replaying {
                    self.stopping = None; // a live interrupt's fallback outlives history
                }
                let cut = std::mem::take(&mut self.cut);
                // A pruned or deleted session: claude says so in the result and exits, and
                // every resume would fail the same way. (Replay restores the session after.)
                let gone = is_error && text.starts_with("No conversation found with session ID");
                let took = duration(secs);
                self.blocks.push(Block::Info(if cut {
                    format!("interrupted · {took} · ${cost:.2}")
                } else if is_error {
                    format!("error: {text}")
                } else {
                    format!("done · {took} · ${cost:.2}")
                }));
                if gone {
                    self.meta.session_id = None;
                    self.blocks.push(Block::Info(
                        "previous session not found; starting fresh, send again".into(),
                    ));
                }
                if !self.replaying {
                    if gone {
                        cx.emit(ChatEvent::Meta);
                    }
                    cx.emit(ChatEvent::TurnDone);
                }
            }
            Ev::Exited(msg) => {
                self.drop_process();
                if self.busy {
                    self.busy = false;
                    self.blocks.push(Block::Info(msg));
                    cx.emit(ChatEvent::TurnDone);
                } else {
                    let first = msg.lines().next().unwrap_or_default();
                    self.blocks.push(Block::Info(format!(
                        "{first} while idle · the next message restarts it"
                    )));
                }
            }
        }
    }

    /// On the last row the list follows new output again; elsewhere it holds still.
    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.sel = ix.min(self.rows.len().saturating_sub(1));
        if self.sel + 1 >= self.rows.len() {
            self.list.set_follow_mode(FollowMode::Tail);
        } else {
            self.list.pause_following_tail();
            self.list.scroll_to_reveal_item(self.sel);
        }
        cx.notify();
    }

    fn render_row(&self, ix: usize, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(&(bi, call)) = self.rows.get(ix) else {
            return div().into_any_element();
        };
        let focused = self.focus.is_focused(window);
        let theme = cx.theme();
        let row = div()
            .id(ix)
            .w_full()
            .px_4()
            .py_1p5()
            .border_l_2()
            .border_color(if focused && ix == self.sel {
                theme.primary
            } else {
                theme.transparent
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.sel = ix;
                this.toggle(cx);
            }));
        // A centered ~70-character column; long lines are harder to track back across.
        let column = || div().w_full().max_w(px(680.)).mx_auto();
        let icon = |name: IconName| Icon::new(name).with_size(px(14.));
        let chevron = |open: bool| {
            icon(if open {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
        };
        let status = |c: &Call| match (&c.result, c.is_error) {
            (None, _) => Spinner::new().with_size(px(14.)).into_any_element(),
            (Some(_), true) => icon(IconName::X)
                .text_color(theme.danger_foreground)
                .into_any_element(),
            _ => icon(IconName::Check).into_any_element(),
        };
        let (mono, mono_size) = (theme.mono_font_family.clone(), theme.mono_font_size);
        let tool_row = |c: &Call| {
            let input: SharedString = c.input.clone().into();
            let (mono, mono_size) = (mono.clone(), mono_size);
            v_flex()
                .child(
                    h_flex()
                        .id(SharedString::from(c.id.clone()))
                        .h(px(28.))
                        .gap_2()
                        .text_color(theme.muted_foreground)
                        .child(chevron(c.open))
                        .when_some(standalone(&c.name), |d, i| d.child(icon(i)))
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.foreground)
                                .child(c.name.clone()),
                        )
                        .child(div().flex_1().truncate().child(c.summary.clone()))
                        .child(status(c))
                        .tooltip(move |w, cx| {
                            let (input, mono) = (input.clone(), mono.clone());
                            Tooltip::element(move |_, _| {
                                div()
                                    .max_w(px(560.))
                                    .py_1()
                                    .font_family(mono.clone())
                                    .text_size(mono_size)
                                    .child(input.clone())
                            })
                            .build(w, cx)
                        }),
                )
                .when(c.open, |d| {
                    // Clipped, with a "truncated" note, when parsed (agent::MAX_OUTPUT).
                    let text = c.result.clone().unwrap_or("(running)".into());
                    d.child(
                        div()
                            .mt_1()
                            .px_3()
                            .py_2()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.muted)
                            .font_family(theme.mono_font_family.clone())
                            .text_size(theme.mono_font_size)
                            .line_height(relative(1.45))
                            .text_color(if c.is_error {
                                theme.danger_foreground
                            } else {
                                theme.foreground
                            })
                            .child(text),
                    )
                })
        };
        let inner = match (&self.blocks[bi], call) {
            (Block::User(t, queued), _) => column()
                .px_3()
                .py_2()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .bg(theme.secondary)
                .text_color(theme.secondary_foreground)
                .text_size(px(14.))
                .line_height(relative(1.5))
                .child(t.clone())
                .when(*queued, |d| {
                    d.child(
                        h_flex()
                            .gap_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(Icon::new(IconName::Clock).with_size(px(12.)))
                            .child("Queued · the agent reads it at its next step"),
                    )
                }),
            (Block::Text { md, .. }, _) => column()
                .text_size(px(14.))
                .line_height(relative(1.5))
                .child(TextView::new(md).selectable(true)),
            (Block::Tool(c), _) => column().text_sm().child(tool_row(c)),
            // Calls sit under the group's summary text, past its chevron and gap.
            (Block::Tools { calls, .. }, Some(j)) => {
                column().text_sm().pl(px(22.)).child(tool_row(&calls[j]))
            }
            (Block::Tools { calls, open }, None) => {
                let running = calls.iter().rev().find(|c| c.result.is_none());
                let failed = calls.iter().filter(|c| c.is_error).count();
                column().text_sm().child(
                    h_flex()
                        .h(px(28.))
                        .gap_2()
                        .text_color(theme.muted_foreground)
                        .child(chevron(*open))
                        .child(div().flex_none().child(group_summary(calls)))
                        // While running, the call in flight; the summary alone hides what's slow.
                        .child(div().flex_1().truncate().when_some(running, |d, c| {
                            d.child(format!("· {} {}", c.name, c.summary))
                        }))
                        .when(failed > 0, |d| {
                            d.child(
                                h_flex()
                                    .gap_1()
                                    .text_color(theme.danger_foreground)
                                    .child(icon(IconName::X))
                                    .child(format!("{failed} failed")),
                            )
                        })
                        .child(match running {
                            Some(c) => status(c),
                            None if failed == 0 => icon(IconName::Check).into_any_element(),
                            None => div().into_any_element(),
                        }),
                )
            }
            (Block::Info(t), _) => column()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(t.clone()),
        };
        row.child(inner).into_any_element()
    }

    /// Queued messages above the input, in send order: the top row goes next.
    fn render_queue(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.queue.is_empty() {
            return None;
        }
        let focused = self.qfocus.is_focused(window);
        let theme = cx.theme();
        let faint = crate::app::faint(theme);
        let action = |id: (&'static str, usize),
                      icon: IconName,
                      tip: &'static str,
                      f: fn(&mut Self, &mut Window, &mut Context<Self>)| {
            let ix = id.1;
            Button::new(id)
                .ghost()
                .xsmall()
                .icon(icon)
                .tooltip(tip)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.qsel = ix;
                    f(this, window, cx);
                    cx.stop_propagation(); // the row's own click would refocus the queue
                }))
        };
        let rows: Vec<AnyElement> = (self.queue.iter().enumerate())
            .map(|(ix, q)| {
                let sel = focused && ix == self.qsel;
                h_flex()
                    .id(("queued", ix))
                    .h(px(28.))
                    .pl_2()
                    .gap_2()
                    .rounded(theme.radius)
                    .border_l_2()
                    .border_color(if sel {
                        theme.primary
                    } else {
                        theme.transparent
                    })
                    .when(sel, |d| d.bg(theme.list_active))
                    .when(!sel, |d| d.hover(|d| d.bg(theme.list_hover)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.qsel = ix;
                        this.focus_queue(window, cx);
                    }))
                    .child(
                        Icon::new(IconName::CornerDownRight)
                            .with_size(px(14.))
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.foreground)
                            .child(q.preview.clone()),
                    )
                    .child(action(
                        ("q-send", ix),
                        IconName::ArrowUp,
                        "Send now (enter)",
                        Self::steer,
                    ))
                    .child(action(
                        ("q-edit", ix),
                        IconName::Pencil,
                        "Edit (e)",
                        Self::edit_queued,
                    ))
                    .child(action(
                        ("q-del", ix),
                        IconName::Trash,
                        "Delete (d)",
                        Self::delete_queued,
                    ))
                    .into_any_element()
            })
            .collect();
        let n = self.queue.len();
        Some(
            v_flex()
                .id("queue")
                .key_context("Queue")
                .track_focus(&self.qfocus)
                .pb_1()
                .mb_1()
                .border_b_1()
                .border_color(theme.border)
                .text_sm()
                .on_action(cx.listener(|this, _: &SelectNext, w, cx| {
                    this.queue_select(this.qsel + 1, w, cx)
                }))
                .on_action(cx.listener(|this, _: &SelectPrev, w, cx| {
                    this.queue_select(this.qsel.saturating_sub(1), w, cx)
                }))
                .on_action(cx.listener(|this, _: &SelectFirst, w, cx| this.queue_select(0, w, cx)))
                .on_action(cx.listener(|this, _: &SelectLast, w, cx| {
                    this.queue_select(this.queue.len().saturating_sub(1), w, cx)
                }))
                .on_action(cx.listener(|this, _: &Confirm, w, cx| this.steer(w, cx)))
                .on_action(cx.listener(|this, _: &EditQueued, w, cx| this.edit_queued(w, cx)))
                .on_action(cx.listener(|this, _: &DeleteQueued, w, cx| this.delete_queued(w, cx)))
                .on_action(cx.listener(|this, _: &MoveQueuedUp, _, cx| this.move_queued(-1, cx)))
                .on_action(cx.listener(|this, _: &MoveQueuedDown, _, cx| this.move_queued(1, cx)))
                .on_action(cx.listener(|this, _: &FocusComposer, w, cx| this.focus_composer(w, cx)))
                .child(
                    h_flex()
                        .h(px(24.))
                        .px_2()
                        .gap_3()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(match self.editing.is_some() {
                            true => format!("{n} queued · paused while you edit"),
                            false => format!("{n} queued · sends when the turn ends"),
                        })
                        .child(div().flex_1())
                        .when(focused, |d| {
                            d.text_color(faint)
                                .child(hint("enter", "send now"))
                                .child(hint("e", "edit"))
                                .child(hint("d", "delete"))
                                .child(hint("shift-k shift-j", "reorder"))
                        })
                        .when(!focused, |d| {
                            d.text_color(faint).child(hint("up", "manage"))
                        }),
                )
                .child(
                    div()
                        .id("queue-rows")
                        .max_h(px(5. * 28.))
                        .overflow_y_scroll()
                        .track_scroll(&self.qscroll)
                        .children(rows),
                )
                .into_any_element(),
        )
    }

    fn toggle(&mut self, cx: &mut Context<Self>) {
        let Some(&(bi, call)) = self.rows.get(self.sel) else {
            return;
        };
        match (&mut self.blocks[bi], call) {
            (Block::Tool(c), _) => c.open = !c.open,
            (Block::Tools { calls, .. }, Some(j)) => calls[j].open = !calls[j].open,
            (Block::Tools { open, .. }, None) => *open = !*open,
            _ => return,
        }
        self.sync_rows(Some(bi));
        cx.notify();
    }
}

impl Render for Chat {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The queue emptied (a turn ended, a git step finished) while it had focus.
        if self.queue.is_empty() && self.qfocus.is_focused(window) {
            self.focus_composer(window, cx);
        }
        let theme = cx.theme();
        let (radius, border, card, muted) = (
            theme.radius_lg,
            theme.border,
            theme.secondary,
            theme.muted_foreground,
        );
        let rows = list(
            self.list.clone(),
            cx.processor(|this, ix, window, cx| this.render_row(ix, window, cx)),
        )
        .size_full()
        .py_2();
        let drop_bg = theme.drop_target;
        let accent = theme.drag_border;
        v_flex()
            .id("chat-pane")
            .group("chat-pane")
            .relative()
            .size_full()
            // Files from Finder anywhere on the pane are copied in and mentioned in the composer.
            .on_drop(cx.listener(|this, p: &ExternalPaths, window, cx| {
                this.attach(p.paths().to_vec(), None, window, cx)
            }))
            .child(
                div()
                    .id("transcript")
                    .key_context("Transcript")
                    .track_focus(&self.focus)
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .on_action(
                        cx.listener(|this, _: &SelectNext, _, cx| this.select(this.sel + 1, cx)),
                    )
                    .on_action(cx.listener(|this, _: &SelectPrev, _, cx| {
                        this.select(this.sel.saturating_sub(1), cx)
                    }))
                    .on_action(cx.listener(|this, _: &SelectFirst, _, cx| this.select(0, cx)))
                    .on_action(
                        cx.listener(|this, _: &SelectLast, _, cx| this.select(usize::MAX, cx)),
                    )
                    .on_action(
                        cx.listener(|this, _: &PageDown, _, cx| this.select(this.sel + 5, cx)),
                    )
                    .on_action(cx.listener(|this, _: &PageUp, _, cx| {
                        this.select(this.sel.saturating_sub(5), cx)
                    }))
                    .on_action(cx.listener(|this, _: &Confirm, _, cx| this.toggle(cx)))
                    .on_action(cx.listener(|this, _: &FocusComposer, window, cx| {
                        this.focus_composer(window, cx)
                    }))
                    .child(rows)
                    .vertical_scrollbar(&self.list),
            )
            .child(
                div()
                    .px_4()
                    .pb_4()
                    .pt_1()
                    .children(self.render_side(window, cx))
                    .child(
                        v_flex()
                            .key_context("Composer")
                            .relative()
                            .w_full()
                            .max_w(px(712.))
                            .mx_auto()
                            .p_2()
                            .gap_1()
                            .rounded(radius)
                            .border_1()
                            .border_color(border)
                            .bg(card)
                            .on_action(cx.listener(|this, _: &FocusTranscript, window, cx| {
                                this.focus.focus(window, cx);
                                cx.notify();
                            }))
                            .on_action(cx.listener(|this, _: &AskSide, window, cx| {
                                this.ask_draft_aside(window, cx)
                            }))
                            // While the menu is open, these keys drive it instead of the input.
                            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                                if this.menu.is_some() {
                                    this.move_menu(1, cx);
                                    cx.stop_propagation();
                                }
                            }))
                            .capture_action(cx.listener(|this, _: &MoveUp, window, cx| {
                                if this.menu.is_some() {
                                    this.move_menu(-1, cx);
                                    cx.stop_propagation();
                                    return;
                                }
                                // Up from the first line goes to the queue, like Claude Code's.
                                let c = this.composer.read(cx);
                                let first_line = !c.value()[..c.cursor()].contains('\n');
                                if first_line
                                    && !this.queue.is_empty()
                                    && c.focus_handle(cx).is_focused(window)
                                {
                                    this.qsel = this.queue.len() - 1;
                                    this.focus_queue(window, cx);
                                    cx.stop_propagation();
                                }
                            }))
                            .capture_action(cx.listener(|this, a: &Enter, window, cx| {
                                // With no match, Enter sends what was typed.
                                if !a.shift && this.pick_selected(window, cx) {
                                    cx.stop_propagation();
                                }
                            }))
                            .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                                if this.pick_selected(window, cx) {
                                    cx.stop_propagation();
                                }
                            }))
                            .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                                if this.menu.is_some() {
                                    this.close_menu(cx);
                                    cx.stop_propagation();
                                }
                            }))
                            .when_some(self.render_menu(cx), |d, menu| d.child(menu))
                            .when_some(self.render_queue(window, cx), |d, q| d.child(q))
                            .child(
                                Textarea::new(&self.composer)
                                    .on_paste({
                                        let chat = cx.entity().downgrade();
                                        move |item, window, cx| {
                                            chat.update(cx, |c, cx| c.paste(item, window, cx))
                                                .unwrap_or(false)
                                        }
                                    })
                                    .appearance(false)
                                    .text_size(px(14.)),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(
                                        Button::new("model")
                                            .ghost()
                                            .xsmall()
                                            .icon(IconName::Sparkles)
                                            .label(self.model().to_string())
                                            .dropdown_caret(true)
                                            .tooltip_with_action(
                                                match &self.reported {
                                                    Some(r) => format!("Change model · {r}"),
                                                    None => "Change model".into(),
                                                },
                                                &PickModel,
                                                None,
                                            )
                                            .on_click(|_, window, cx| {
                                                window.dispatch_action(PickModel.boxed_clone(), cx)
                                            }),
                                    )
                                    .child(div().flex_1())
                                    .child(Kbd::new(Keystroke::parse("enter").unwrap()))
                                    .child(match (self.editing.is_some(), self.busy || self.hold) {
                                        (true, true) => "save",
                                        (false, true) => "queue",
                                        _ => "send",
                                    })
                                    .when(self.busy && !self.hold, |d| {
                                        d.child(Kbd::new(Keystroke::parse("cmd-enter").unwrap()))
                                            .child("send now")
                                    })
                                    .when(self.busy, |d| {
                                        d.child(Kbd::new(Keystroke::parse("ctrl-c").unwrap()))
                                            .child("interrupt")
                                    })
                                    .child(Kbd::new(Keystroke::parse("cmd-shift-enter").unwrap()))
                                    .child("side question"),
                            ),
                    ),
            )
            // No hitbox, so it never takes the drop or the mouse; shown only while files hover.
            .child(
                div()
                    .absolute()
                    .inset_2()
                    .invisible()
                    .group_drag_over::<ExternalPaths>("chat-pane", |s| s.visible())
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(radius)
                    .border_2()
                    .border_dashed()
                    .border_color(accent)
                    .bg(drop_bg)
                    .text_color(muted)
                    .child("Drop to attach"),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin

    #[test]
    fn summarizes_tool_groups() {
        let call = |name: &str, err| Call {
            id: String::new(),
            name: name.into(),
            summary: String::new(),
            input: String::new(),
            result: Some("".into()),
            is_error: err,
            open: false,
        };
        let calls = ["Read", "Bash", "Read", "Edit", "Write", "mcp__x__y"].map(|n| call(n, false));
        assert_eq!(
            group_summary(&calls),
            "Read 2 files, ran 1 command, edited 2 files, used 1 tool"
        );
        assert_eq!(group_summary(&[]), "");
        let input = serde_json::json!({"file_path": "/ws/feat/api/src/a.rs"});
        assert_eq!(summarize(&input, "/ws/feat"), "api/src/a.rs");
        let skill = serde_json::json!({"skill": "pdf", "args": "x"});
        assert_eq!(summarize(&skill, "/ws/feat"), "pdf");
        let bash = serde_json::json!({"command": "a\nb", "timeout": 5});
        assert_eq!(full_input(&bash), "command: a\nb\ntimeout: 5");
        assert_eq!(cap("abcdef", 3), "abc…");
    }
}

#[cfg(all(test, feature = "test-support"))]
mod ui_tests {
    use super::*;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin
    use gpui_kit::InputEvent as _;
    use gpui_kit::test::TestWindowExt as _;

    /// A claude stand-in that swallows stdin. Side questions on a session (`--fork-session`)
    /// get a canned answer, and their args are written next to the script.
    const FAKE: &str = r#"#!/bin/sh
case "$*" in *--fork-session*)
  echo "$*" > "$0.args"
  read line
  echo '{"type":"stream_event","event":{"delta":{"type":"text_delta","text":"Forty"}},"parent_tool_use_id":null}'
  echo '{"type":"assistant","message":{"content":[{"type":"text","text":"Forty-two"}]},"parent_tool_use_id":null}'
  echo '{"type":"result","is_error":false,"result":"Forty-two","total_cost_usd":0,"duration_ms":1}'
esac
exec cat >/dev/null
"#;

    /// Installs the script once for the whole test run: tests run in parallel, and one must
    /// never exec a script another is still writing.
    fn fake_claude() -> std::path::PathBuf {
        static FAKE_PATH: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
        FAKE_PATH
            .get_or_init(|| {
                let dir = std::env::temp_dir().join(format!("shark-fake-{}", std::process::id()));
                std::fs::create_dir_all(&dir).unwrap();
                let fake = dir.join("claude");
                std::fs::write(&fake, FAKE).unwrap();
                std::process::Command::new("chmod")
                    .args(["+x"])
                    .arg(&fake)
                    .status()
                    .unwrap();
                // SAFETY: set once, before any test spawns claude.
                unsafe { std::env::set_var("SHARK_CLAUDE", &fake) };
                fake
            })
            .clone()
    }

    /// A side question forks the chat's session with no tools and nothing saved.
    #[test]
    fn side_question_process() {
        let root = std::env::temp_dir().join(format!("shark-side-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let fake = fake_claude();
        let ws = Workspace {
            name: format!("side-proc-{}", std::process::id()),
            ..Default::default()
        };
        let meta = ChatMeta {
            id: ws.name.clone(),
            session_id: Some("s1".into()),
            ..Default::default()
        };
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::fs::create_dir_all(crate::state::workspaces_dir()).unwrap();
        let _ = std::fs::remove_file(ws.dir());
        std::os::unix::fs::symlink(&root, ws.dir()).unwrap();
        let (mut a, rx) = agent::spawn_side(&ws, &meta).unwrap();
        a.send("what is x?").unwrap();
        let mut text = String::new();
        while let Ok(ev) = rx.recv_blocking() {
            match ev {
                Ev::Text(t) => text = t,
                Ev::Result { .. } => break,
                Ev::Exited(e) => panic!("{e}"),
                _ => {}
            }
        }
        assert_eq!(text, "Forty-two");
        let args = std::fs::read_to_string(fake.with_extension("args")).unwrap();
        assert!(args.contains("--resume s1 --fork-session"), "{args}");
        assert!(
            args.contains("--tools  --strict-mcp-config"),
            "no tools: {args}"
        );
        assert!(args.contains("--no-session-persistence"), "{args}");
        assert!(!agent::transcript_path(&meta.id).exists(), "no transcript");
        let _ = std::fs::remove_file(ws.dir());
    }

    /// cmd-shift-enter asks the draft on the side: the answer streams into the card, the chat
    /// never sees it, and enter puts the answer into the composer.
    #[gpui_kit::test]
    fn side_question(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::bind_keys(cx);
        });
        let root = std::env::temp_dir().join(format!("shark-side-ui-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        fake_claude();
        let ws = Workspace {
            name: format!("side-test-{}", std::process::id()),
            ..Default::default()
        };
        // No session: the fake stays quiet, and the test plays the answer in (a reply from a
        // real thread would trip the deterministic scheduler).
        let meta = ChatMeta {
            id: ws.name.clone(),
            ..Default::default()
        };
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::fs::create_dir_all(crate::state::workspaces_dir()).unwrap();
        let _ = std::fs::remove_file(ws.dir());
        std::os::unix::fs::symlink(&root, ws.dir()).unwrap();
        let (window, chat) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
                    cx.new(|cx| Chat::new(ws.clone(), meta, w, cx))
                })
            })
            .unwrap();
        let act = |cx: &mut TestAppContext, f: &dyn Fn(&mut Window, &mut App)| {
            cx.update_window(window, |_, w, cx| {
                f(w, cx);
                w.render_frame(cx);
            })
            .unwrap()
        };
        act(cx, &|w, cx| {
            chat.update(cx, |c, cx| c.focus_composer(w, cx))
        });
        act(cx, &|w, cx| w.input("what is x?", cx));
        act(cx, &|w, cx| w.press("cmd-shift-enter", cx));
        let result = || Ev::Result {
            is_error: false,
            text: String::new(),
            cost: 0.,
            secs: 0,
        };
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                // The fork first finishes a turn for a task notification left in the session.
                c.apply_side(Ev::Text("notification noise".into()), cx);
                c.apply_side(result(), cx);
                let side = c.side.as_ref().unwrap();
                assert!(!side.done && side.text.is_empty(), "not the answer yet");
                c.apply_side(Ev::User("what is x?".into()), cx);
                c.apply_side(Ev::TextDelta("Forty".into()), cx);
                c.apply_side(Ev::Text("Forty-two".into()), cx);
                c.apply_side(result(), cx);
            })
        });
        act(cx, &|_, _| {});
        cx.update(|cx| {
            let c = chat.read(cx);
            let side = c.side.as_ref().unwrap();
            assert_eq!(side.question, "what is x?");
            assert_eq!(side.text, "Forty-two");
            assert!(side.done && side.agent.is_none() && side.error.is_none());
            assert!(c.blocks.is_empty(), "the chat never sees the side question");
            assert!(!c.busy);
            assert_eq!(c.composer.read(cx).value().as_ref(), "");
        });

        act(cx, &|w, cx| w.press("enter", cx));
        cx.update(|cx| {
            let c = chat.read(cx);
            assert!(c.side.is_none());
            assert_eq!(c.composer.read(cx).value().as_ref(), "Forty-two");
        });

        // Opened empty, it has its own field; esc closes it and the draft stays.
        act(cx, &|w, cx| chat.update(cx, |c, cx| c.open_side(w, cx)));
        act(cx, &|w, cx| w.input("ignored", cx));
        act(cx, &|w, cx| w.press("escape", cx));
        cx.update(|cx| {
            let c = chat.read(cx);
            assert!(c.side.is_none());
            assert_eq!(c.composer.read(cx).value().as_ref(), "Forty-two");
        });

        // An error before the echo (the fork's session is gone) still ends the card with it.
        act(cx, &|w, cx| w.press("cmd-shift-enter", cx));
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                c.apply_side(
                    Ev::Result {
                        is_error: true,
                        text: "No conversation found".into(),
                        cost: 0.,
                        secs: 0,
                    },
                    cx,
                );
                let side = c.side.as_ref().unwrap();
                assert!(side.done && side.error.as_deref() == Some("No conversation found"));
            })
        });
        let _ = std::fs::remove_file(ws.dir());
    }

    /// Sending jumps to the bottom even after scrolling the transcript up.
    #[gpui_kit::test]
    fn send_scrolls_to_bottom(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let ws = Workspace {
            name: format!("scroll-test-{}", std::process::id()),
            branch: "b".into(),
            repos: vec![],
            ..Default::default()
        };
        let meta = ChatMeta {
            id: ws.name.clone(),
            ..Default::default()
        };
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(0.), px(0.)),
                size(px(800.), px(400.)),
            ))),
            ..Default::default()
        };
        let (window, chat) = cx
            .update(|cx| {
                gpui_kit::open_window(options, cx, |w, cx| cx.new(|cx| Chat::new(ws, meta, w, cx)))
            })
            .unwrap();
        let frame = |cx: &mut TestAppContext| {
            cx.update_window(window, |_, w, cx| w.render_frame(cx))
                .unwrap()
        };
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                (0..40).for_each(|i| c.apply(Ev::User(format!("message {i}")), cx))
            })
        });
        frame(cx);
        let list = cx.update(|cx| chat.read(cx).list.clone());
        let top = || list.logical_scroll_top().item_ix;
        assert!(
            list.is_following_tail() && top() > 0,
            "overflows, at the bottom"
        );
        cx.update(|cx| chat.update(cx, |c, cx| c.select(0, cx)));
        frame(cx);
        assert_eq!(top(), 0, "scrolled to the top");
        // New output doesn't pull a reader back down.
        cx.update(|cx| chat.update(cx, |c, cx| c.apply(Ev::Text("more".into()), cx)));
        frame(cx);
        assert_eq!(top(), 0, "still at the top");

        cx.update(|cx| chat.update(cx, |c, cx| c.sent("new".into(), cx)));
        frame(cx);
        assert!(list.is_following_tail() && top() > 0, "at the bottom");

        // Scrolling up by wheel, without moving the cursor, also stops following.
        list.scroll_by(px(-100.));
        frame(cx);
        let read_at = top();
        cx.update(|cx| chat.update(cx, |c, cx| c.apply(Ev::Text("reply".into()), cx)));
        frame(cx);
        assert!(
            !list.is_following_tail() && top() == read_at,
            "stays scrolled up"
        );
    }

    /// A chat on a fresh workspace resuming session `s1`.
    fn open_chat(tag: &str, cx: &mut TestAppContext) -> Entity<Chat> {
        cx.update(gpui_kit::init);
        let ws = Workspace {
            name: format!("{tag}-test-{}", std::process::id()),
            branch: "b".into(),
            repos: vec![],
            ..Default::default()
        };
        // Titled, so a first send emits nothing.
        let meta = ChatMeta {
            id: ws.name.clone(),
            title: tag.into(),
            session_id: Some("s1".into()),
            ..Default::default()
        };
        let (_, chat) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
                    cx.new(|cx| Chat::new(ws, meta, w, cx))
                })
            })
            .unwrap();
        chat
    }

    /// Calls cut off by an exit stop spinning; a missing session is dropped, an early exit isn't.
    #[gpui_kit::test]
    fn exits_end_calls_and_drop_missing_sessions(cx: &mut TestAppContext) {
        let chat = open_chat("exit", cx);
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                c.busy = true;
                let input = serde_json::json!({"command": "sleep 99"});
                c.apply(
                    Ev::ToolUse {
                        id: "t".into(),
                        name: "Bash".into(),
                        input,
                    },
                    cx,
                );
                c.apply(Ev::Exited("claude exited (signal: 15)".into()), cx);
                let Block::Tools { calls, .. } = &c.blocks[0] else {
                    panic!()
                };
                assert_eq!(calls[0].result.as_deref(), Some("(interrupted)"));
                assert!(calls[0].is_error);
                assert_eq!(
                    c.meta.session_id.as_deref(),
                    Some("s1"),
                    "an exit keeps the session"
                );

                c.busy = true;
                let text = "No conversation found with session ID: s1".into();
                c.apply(
                    Ev::Result {
                        is_error: true,
                        text,
                        cost: 0.,
                        secs: 0,
                    },
                    cx,
                );
                assert_eq!(c.meta.session_id, None);

                c.meta.session_id = Some("s2".into());
                c.new_conversation(cx);
                assert_eq!(c.meta.session_id, None);
                assert!(!c.working());
            })
        });
    }

    /// A mid-turn send shows as queued until claude echoes it; replay marks a turn cut off
    /// by quitting, goes ahead of live blocks, and emits nothing.
    #[gpui_kit::test]
    fn queues_mid_turn_and_replays_quietly(cx: &mut TestAppContext) {
        let chat = open_chat("steer", cx);
        let emitted = std::rc::Rc::new(std::cell::Cell::new(0));
        let n = emitted.clone();
        cx.update(|cx| {
            cx.subscribe(&chat, move |_, _, _| n.set(n.get() + 1))
                .detach()
        });
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                c.sent("first".into(), cx);
                c.sent("steer".into(), cx);
                let queued = |c: &Chat| {
                    (c.blocks.iter())
                        .map(|b| matches!(b, Block::User(_, true)))
                        .collect::<Vec<_>>()
                };
                assert_eq!(queued(c), [false, true], "only the mid-turn send is queued");
                c.apply(Ev::User("first".into()), cx);
                c.blocks.remove(1);
                c.unread.pop_back();
                c.apply(Ev::TextDelta("Hel".into()), cx);
                c.sent("steer".into(), cx);
                c.apply(Ev::TextDelta("lo".into()), cx);
                c.apply(Ev::Text("Hello".into()), cx);
                assert!(
                    matches!(&c.blocks[1], Block::Text { done: true, .. }),
                    "a message queued mid-stream doesn't split the reply"
                );
                assert_eq!(c.blocks.len(), 3);
                c.apply(Ev::User("steer".into()), cx);
                assert_eq!(queued(c), [false, false, false], "echoes don't duplicate");
                c.blocks.remove(1);

                let call = Ev::ToolUse {
                    id: "t".into(),
                    name: "Bash".into(),
                    input: serde_json::json!({}),
                };
                let init = Ev::Init {
                    session_id: "old".into(),
                    model: String::new(),
                    permission_mode: String::new(),
                    mcp: vec![],
                };
                let history = vec![init, Ev::User("old".into()), call];
                c.load_history(history, cx);
                let Block::Info(t) = &c.blocks[2] else {
                    panic!()
                };
                assert_eq!(t, "interrupted");
                assert!(matches!(&c.blocks[3], Block::User(t, _) if t == "first"));
                assert_eq!(c.meta.session_id.as_deref(), Some("s1"));
                assert!(c.busy, "the live turn still runs");

                c.apply(Ev::Interrupted, cx);
                let result = Ev::Result {
                    is_error: true,
                    text: String::new(),
                    cost: 0.,
                    secs: 3,
                };
                c.apply(result, cx);
                let Some(Block::Info(t)) = c.blocks.last() else {
                    panic!()
                };
                assert_eq!(t, "interrupted · 3s · $0.00");
            })
        });
        assert_eq!(emitted.get(), 1, "only the live TurnDone");
    }

    /// While a turn runs Enter queues; the queue is driven from the keyboard, steers with
    /// enter / cmd-enter, and drains on a good turn end but not after an error.
    #[gpui_kit::test]
    fn queue_while_running(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::bind_keys(cx);
        });
        let root = std::env::temp_dir().join(format!("shark-queue-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        fake_claude();
        let ws = Workspace {
            name: format!("queue-test-{}", std::process::id()),
            branch: "b".into(),
            repos: vec![],
            ..Default::default()
        };
        // The UI tests reset the test home; don't let one do that under this workspace.
        let meta = ChatMeta {
            id: ws.name.clone(),
            ..Default::default()
        };
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::fs::create_dir_all(crate::state::workspaces_dir()).unwrap();
        std::os::unix::fs::symlink(&root, ws.dir()).unwrap();
        let (dir, transcript) = (ws.dir(), agent::transcript_path(&meta.id));
        let (window, chat) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
                    cx.new(|cx| Chat::new(ws, meta, w, cx))
                })
            })
            .unwrap();
        cx.run_until_parked();
        let act = |cx: &mut TestAppContext, f: &dyn Fn(&mut Window, &mut App)| {
            cx.update_window(window, |_, w, cx| {
                f(w, cx);
                w.render_frame(cx); // the queue's focus target exists only once rendered
            })
            .unwrap()
        };
        let queue = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                (chat.read(cx).queue.iter())
                    .map(Queued::text)
                    .collect::<Vec<_>>()
            })
        };
        let qsel = |cx: &mut TestAppContext| cx.update(|cx| chat.read(cx).qsel);
        let value = |cx: &mut TestAppContext| {
            cx.update(|cx| chat.read(cx).composer.read(cx).value().to_string())
        };
        let users = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                (chat.read(cx).blocks.iter())
                    .filter_map(|b| match b {
                        Block::User(t, _) => Some(t.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
        };
        let in_queue = |cx: &mut TestAppContext| {
            cx.update_window(window, |_, w, cx| chat.read(cx).qfocus.is_focused(w))
                .unwrap()
        };
        let result = |is_error| Ev::Result {
            is_error,
            text: String::new(),
            cost: 0.,
            secs: 1,
        };

        cx.update(|cx| chat.update(cx, |c, _| c.busy = true));
        act(cx, &|w, cx| {
            chat.update(cx, |c, cx| c.focus_composer(w, cx))
        });
        act(cx, &|w, cx| w.press("up", cx));
        assert!(!in_queue(cx), "nothing queued: up stays in the composer");
        for m in ["first", "second", "third"] {
            act(cx, &|w, cx| w.input(m, cx));
            act(cx, &|w, cx| w.press("enter", cx));
        }
        assert_eq!(queue(cx), ["first", "second", "third"]);
        assert_eq!(value(cx), "", "a queued message leaves the composer");
        assert!(users(cx).is_empty(), "nothing sent while running");

        act(cx, &|w, cx| w.press("up", cx));
        assert!(in_queue(cx), "up from the composer enters the queue");
        assert_eq!(qsel(cx), 2, "on the row nearest the composer");
        act(cx, &|w, cx| w.press("k", cx));
        act(cx, &|w, cx| w.press("shift-k", cx));
        assert_eq!(queue(cx), ["second", "first", "third"]);
        assert_eq!(qsel(cx), 0, "the cursor follows the moved row");
        act(cx, &|w, cx| w.press("shift-k", cx));
        assert_eq!(qsel(cx), 0, "the top row can't move up");
        act(cx, &|w, cx| w.press("alt-down", cx));
        assert_eq!(queue(cx), ["first", "second", "third"]);

        // Edit: back into the composer, then into the same slot.
        act(cx, &|w, cx| w.press("e", cx));
        assert!(!in_queue(cx));
        assert_eq!(value(cx), "second");
        assert_eq!(queue(cx), ["first", "third"]);
        act(cx, &|w, cx| w.input(" edited", cx));
        cx.update(|cx| chat.update(cx, |c, cx| c.apply(result(false), cx)));
        assert!(
            users(cx).is_empty(),
            "the queue waits while a message is edited"
        );
        cx.update(|cx| chat.update(cx, |c, _| c.busy = true));
        act(cx, &|w, cx| w.press("enter", cx));
        assert_eq!(queue(cx), ["first", "second edited", "third"]);

        act(cx, &|w, cx| w.press("up", cx));
        act(cx, &|w, cx| w.press("d", cx));
        assert_eq!(queue(cx), ["first", "second edited"]);
        assert_eq!(qsel(cx), 1, "the cursor stays on the last row");
        act(cx, &|w, cx| w.press("down", cx));
        assert!(
            !in_queue(cx),
            "down past the last row returns to the composer"
        );

        // A good turn end sends the top message; an error doesn't.
        cx.update(|cx| chat.update(cx, |c, cx| c.apply(result(false), cx)));
        assert_eq!(users(cx), ["first"]);
        assert_eq!(queue(cx), ["second edited"]);
        cx.update(|cx| assert!(chat.read(cx).busy));
        cx.update(|cx| chat.update(cx, |c, cx| c.apply(result(true), cx)));
        assert_eq!(queue(cx), ["second edited"], "an error pauses the queue");

        // Steer: enter on a row and cmd-enter in the composer send while the turn runs.
        cx.update(|cx| chat.update(cx, |c, _| c.busy = true));
        act(cx, &|w, cx| w.press("up", cx));
        act(cx, &|w, cx| w.press("enter", cx));
        assert_eq!(users(cx), ["first", "second edited"]);
        assert!(queue(cx).is_empty());
        assert!(
            !in_queue(cx),
            "an empty queue hands focus back to the composer"
        );
        act(cx, &|w, cx| w.input("now", cx));
        act(cx, &|w, cx| w.press("cmd-enter", cx));
        assert_eq!(users(cx), ["first", "second edited", "now"]);
        cx.update(|cx| assert!(chat.read(cx).busy, "steering doesn't end the turn"));

        // Sending a row above the one being edited keeps the edit's slot.
        for m in ["a", "b", "c"] {
            act(cx, &|w, cx| w.input(m, cx));
            act(cx, &|w, cx| w.press("enter", cx));
        }
        act(cx, &|w, cx| w.press("up", cx));
        act(cx, &|w, cx| w.press("k", cx));
        act(cx, &|w, cx| w.press("e", cx));
        assert_eq!(value(cx), "b");
        act(cx, &|w, cx| w.press("escape", cx));
        act(cx, &|w, cx| chat.update(cx, |c, cx| c.focus_queue(w, cx)));
        act(cx, &|w, cx| w.press("g", cx));
        act(cx, &|w, cx| w.press("g", cx));
        act(cx, &|w, cx| w.press("enter", cx));
        assert_eq!(queue(cx), ["c"]);
        act(cx, &|w, cx| {
            chat.update(cx, |c, cx| c.focus_composer(w, cx))
        });
        act(cx, &|w, cx| w.press("enter", cx));
        assert_eq!(queue(cx), ["b", "c"], "b keeps its place ahead of c");

        cx.update(|cx| chat.update(cx, |c, _| c.agent = None));
        let _ = std::fs::remove_file(dir);
        let _ = std::fs::remove_file(transcript);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// `@` opens the file menu; keys drive it and a pick leaves a repo-qualified chip.
    #[gpui_kit::test]
    fn dropping_a_file_attaches_it(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::bind_keys(cx);
        });
        let ws = Workspace {
            name: format!("drop-test-{}", std::process::id()),
            branch: "b".into(),
            ..Default::default()
        };
        let (dir, src) = (
            ws.dir(),
            std::env::temp_dir().join(format!("shark-drop-{}", std::process::id())),
        );
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&src).unwrap();
        let shot = src.join("Shot at 10.22\u{202F}AM.png");
        std::fs::write(&shot, "png").unwrap();
        let meta = ChatMeta {
            id: ws.name.clone(),
            ..Default::default()
        };
        let (window, chat) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
                    cx.new(|cx| Chat::new(ws, meta, w, cx))
                })
            })
            .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            let size = w.viewport_size();
            let position = point(size.width / 2., size.height / 2.);
            let paths = ExternalPaths(vec![shot.clone()].into());
            w.dispatch_event(
                FileDropEvent::Entered { position, paths }.to_platform_input(),
                cx,
            );
            w.dispatch_event(FileDropEvent::Submit { position }.to_platform_input(), cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            let c = chat.read(cx).composer.read(cx);
            assert_eq!(
                c.value().as_ref() as &str,
                "@\".context/attachments/Shot at 10.22 AM.png\" "
            );
            assert_eq!(c.tokens().len(), 1);
        });
        assert!(
            dir.join(".context/attachments/Shot at 10.22 AM.png")
                .exists()
        );
        std::fs::remove_dir_all(dir).unwrap();
        std::fs::remove_dir_all(src).unwrap();
    }

    #[gpui_kit::test]
    fn at_mentions_pick_a_file(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::bind_keys(cx);
        });
        let root = std::env::temp_dir().join(format!("shark-at-{}", std::process::id()));
        let mut repos = vec![];
        for name in ["api", "web"] {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("README.md"), "").unwrap();
            crate::git::git(&dir, &["init", "-q"]).unwrap();
            repos.push((name.to_string(), dir));
        }
        let repo = |n: &str| crate::state::WsRepo {
            name: n.into(),
            src: root.join(n),
            base: "origin/main".into(),
        };
        let ws = Workspace {
            name: format!("at-test-{}", std::process::id()),
            branch: "b".into(),
            repos: vec![repo("api"), repo("web")],
            ..Default::default()
        };
        let meta = ChatMeta {
            id: ws.name.clone(),
            ..Default::default()
        };
        let (window, chat) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
                    cx.new(|cx| Chat::new(ws, meta, w, cx))
                })
            })
            .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            chat.update(cx, |c, _| {
                c.files = complete::files(&repos);
                c.indexing = Some(Task::ready(())); // the workspace dir doesn't exist; keep these
            })
        });
        let act = |cx: &mut TestAppContext, f: &dyn Fn(&mut Window, &mut App)| {
            cx.update_window(window, |_, w, cx| f(w, cx)).unwrap()
        };
        act(cx, &|w, cx| {
            chat.update(cx, |c, cx| c.focus_composer(w, cx))
        });
        act(cx, &|w, cx| w.input("see @readme", cx));
        let menu = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                chat.read(cx)
                    .menu
                    .as_ref()
                    .map(|m| (m.matches.len(), m.sel))
            })
        };
        assert_eq!(menu(cx), Some((2, 0)), "both READMEs");
        act(cx, &|w, cx| w.press("down", cx));
        assert_eq!(menu(cx), Some((2, 1)));
        act(cx, &|w, cx| w.press("enter", cx));
        let value = |cx: &mut TestAppContext| {
            cx.update(|cx| chat.read(cx).composer.read(cx).value().to_string())
        };
        assert_eq!(
            value(cx),
            "see @web/README.md ",
            "enter picks, doesn't send"
        );
        assert_eq!(menu(cx), None);
        cx.update(|cx| assert_eq!(chat.read(cx).composer.read(cx).tokens().len(), 1));

        act(cx, &|w, cx| w.input("@ap", cx));
        assert!(menu(cx).is_some());
        act(cx, &|w, cx| w.press("escape", cx));
        assert_eq!(menu(cx), None, "escape closes the menu");
        act(cx, &|w, cx| {
            assert!(
                !chat.read(cx).focus.is_focused(w),
                "and stays in the composer"
            )
        });
        std::fs::remove_dir_all(root).unwrap();
    }
}
