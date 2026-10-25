//! One agent conversation (a chat tab): transcript blocks, composer, and the claude process.
use crate::agent::{self, Agent, Ev};
use crate::complete::{self, Item, Kind};
use crate::state::{ChatMeta, Workspace};
use crate::*;
use gpui_kit::assets::IconName;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{
    Enter, Escape, IndentInline, InlineToken, InputContent, InputEvent, MoveDown, MoveUp, Textarea,
    TextareaState,
};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::{ActiveTheme, Icon, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

struct Call {
    id: String,
    name: String,
    summary: String,
    result: Option<String>,
    is_error: bool,
    open: bool,
}

enum Block {
    User(String),
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

/// The open `@`/`/` completion menu.
struct Menu {
    /// Bytes of the `@query` in the composer that a pick replaces.
    range: std::ops::Range<usize>,
    command: bool,
    matches: Vec<usize>,
    sel: usize,
}

const MENU_ROWS: usize = 50;

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

/// A side question and its answer, in a card above the composer. A forked, tool-less claude
/// answers it; neither the question nor the answer reaches the chat or its transcript.
struct Side {
    /// The card's own question field when opened empty; `None` once asked.
    input: Option<Entity<TextareaState>>,
    question: String,
    answer: Entity<TextViewState>,
    /// The answer as markdown, for copy and insert.
    text: String,
    done: bool,
    error: Option<String>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    /// Dropping the card stops the process and its reader.
    agent: Option<Agent>,
    _reader: Option<Task<()>>,
    _sub: Option<Subscription>,
}

pub struct Chat {
    pub ws: Workspace,
    pub meta: ChatMeta,
    blocks: Vec<Block>,
    sel: usize,
    pub busy: bool,
    /// Background tasks (subagents, shells) still running; the turn can end before they do.
    pub background: usize,
    /// Set while a git step runs in these worktrees; sends wait for it.
    pub hold: bool,
    agent: Option<Agent>,
    pub(crate) composer: Entity<TextareaState>,
    /// Model the running process was started with; a mismatch respawns it on the next send.
    agent_model: Option<String>,
    scroll: ScrollHandle,
    pub focus: FocusHandle,
    reader: Option<Task<()>>,
    files: Vec<Item>,
    commands: Vec<Item>,
    menu: Option<Menu>,
    menu_scroll: ScrollHandle,
    indexing: Option<Task<()>>,
    probing: Option<Task<()>>,
    queue: Vec<Queued>,
    qsel: usize,
    qfocus: FocusHandle,
    qscroll: ScrollHandle,
    /// The queue slot of the message being edited in the composer. The queue waits meanwhile,
    /// so the message doesn't lose its turn.
    editing: Option<usize>,
    /// A message was sent into a running turn and claude may not have read it yet.
    steered: bool,
    side: Option<Side>,
    _subs: Vec<Subscription>,
}

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
            sel: 0,
            busy: false,
            background: 0,
            hold: false,
            agent: None,
            composer,
            agent_model: None,
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
            reader: None,
            commands: complete::cached_commands(&ws_name),
            files: vec![],
            menu: None,
            menu_scroll: ScrollHandle::new(),
            indexing: None,
            probing: None,
            queue: vec![],
            qsel: 0,
            qfocus: cx.focus_handle(),
            qscroll: ScrollHandle::new(),
            editing: None,
            steered: false,
            side: None,
            _subs: vec![_sub],
        };
        // Warm both sources now so the first `@` or `/` doesn't wait.
        chat.reindex(cx);
        chat.reprobe(cx);
        // Replay history through the same path as live events.
        let history =
            std::fs::read_to_string(agent::transcript_path(&chat.meta.id)).unwrap_or_default();
        for ev in history.lines().flat_map(agent::parse) {
            chat.apply(ev, cx);
        }
        if chat.meta.title.is_empty()
            && let Some(Block::User(t)) = chat.blocks.iter().find(|b| matches!(b, Block::User(_)))
        {
            chat.meta.title = title_of(t);
        }
        chat.busy = false;
        chat.background = 0;
        chat.sel = chat.rows().len().saturating_sub(1);
        chat.scroll.scroll_to_bottom();
        chat
    }

    /// Whether anything is still going on: a turn, or tasks it left running in the background.
    pub fn working(&self) -> bool {
        self.busy || self.background > 0
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

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    /// Opens, filters or closes the menu from the text before the cursor.
    fn update_menu(&mut self, cx: &mut Context<Self>) {
        let c = self.composer.read(cx);
        let (text, cursor) = (c.value(), c.cursor());
        let found = complete::trigger(&text, cursor).filter(|(r, _)| {
            // Text right after a chip belongs to the chip, not to a new query.
            !c.tokens()
                .iter()
                .any(|t| t.range().start < r.end && r.start < t.range().end)
        });
        let Some((range, command)) = found else {
            return self.close_menu(cx);
        };
        let opening = self.menu.as_ref().is_none_or(|m| m.command != command);
        if opening {
            // Sources may have changed since the last menu; refresh behind the cached list.
            match command {
                true => self.reprobe(cx),
                false => self.reindex(cx),
            }
        }
        let items = if command { &self.commands } else { &self.files };
        let matches = complete::rank(items, &text[range.start + 1..range.end], MENU_ROWS);
        let sel = match &self.menu {
            Some(m) if !opening && m.matches == matches => m.sel,
            _ => 0,
        };
        self.menu = Some(Menu {
            range,
            command,
            matches,
            sel,
        });
        self.menu_scroll.scroll_to_item(sel);
        cx.notify();
    }

    fn menu_item(&self, ix: usize) -> Option<&Item> {
        let m = self.menu.as_ref()?;
        let items = if m.command {
            &self.commands
        } else {
            &self.files
        };
        items.get(*m.matches.get(ix)?)
    }

    fn move_menu(&mut self, d: isize, cx: &mut Context<Self>) {
        if let Some(m) = &mut self.menu
            && !m.matches.is_empty()
        {
            m.sel = (m.sel as isize + d).rem_euclid(m.matches.len() as isize) as usize;
            self.menu_scroll.scroll_to_item(m.sel);
            cx.notify();
        }
    }

    /// Replaces the query with the picked item as a chip, plus a space to keep typing.
    fn pick(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.menu_item(ix).cloned() else {
            return self.close_menu(cx);
        };
        let range = self.menu.take().unwrap().range;
        // Two repos can both have README.md; the chip names the repo too.
        let label = match item.kind {
            Kind::File | Kind::Dir if self.ws.repos.len() > 1 && !item.detail.is_empty() => {
                format!("{} · {}", item.label, item.badge)
            }
            _ => item.label.clone(),
        };
        let token = InlineToken::new(item.insert.clone(), item.insert.clone()).with_label(label);
        self.composer.update(cx, |c, cx| {
            c.set_selected_range(range, cx);
            if c.replace_with_token(token, window, cx).is_err() {
                // ponytail: tokens reject control characters; such a path goes in as plain text
                c.replace(item.insert.clone(), window, cx);
            }
            c.insert(" ", window, cx);
        });
        cx.notify();
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
            let at = self
                .editing
                .unwrap_or(self.queue.len())
                .min(self.queue.len());
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
            let at = self
                .editing
                .unwrap_or(self.queue.len())
                .min(self.queue.len());
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

    /// Returns whether the text went out; while a turn runs only if `steer`.
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
        // Never mid-turn: a steer would kill the turn it means to steer.
        if self.agent.is_some()
            && self.agent_model != self.meta.model
            && !self.busy
            && !self.steered
        {
            self.agent = None; // drop stops it; the respawn resumes the same session on the new model
        }
        if self.agent.is_none() {
            match agent::spawn(&self.ws, &self.meta) {
                Ok((a, rx)) => {
                    self.agent = Some(a);
                    self.agent_model = self.meta.model.clone();
                    self.reader = Some(cx.spawn(async move |this, cx| {
                        while let Ok(ev) = rx.recv().await {
                            if this.update(cx, |c, cx| c.apply(ev, cx)).is_err() {
                                break;
                            }
                        }
                    }));
                }
                Err(e) => {
                    self.push(Block::Info(format!("failed to start claude: {e}")), cx);
                    return false;
                }
            }
        }
        if let Err(e) = self.agent.as_mut().unwrap().send(&text) {
            self.push(Block::Info(format!("send failed: {e}")), cx);
            return false;
        }
        self.sent(text, cx);
        true
    }

    fn sent(&mut self, text: String, cx: &mut Context<Self>) {
        self.busy = true;
        if self.meta.title.is_empty() {
            self.meta.title = title_of(&text);
            cx.emit(ChatEvent::Meta);
        }
        // Jump to the end so push follows the new message and the reply.
        self.sel = self.rows().len().saturating_sub(1);
        self.push(Block::User(text), cx);
    }

    /// cmd-shift-enter: asks the draft as a side question, or opens the card's own field when
    /// the composer is empty.
    fn ask_draft_aside(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().trim().to_string();
        if text.is_empty() {
            return self.open_side(window, cx);
        }
        self.composer
            .update(cx, |c, cx| c.set_value("", window, cx));
        self.ask_side(text, window, cx);
    }

    /// Opens the side-question card with an empty field, keeping any draft in the composer.
    pub fn open_side(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 4)
                .placeholder("Ask a side question (the chat won't see it)")
                .submit_on_enter(true)
        });
        let sub = cx.subscribe_in(
            &input,
            window,
            |this, input, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = ev {
                    let q = input.read(cx).value().trim().to_string();
                    if !q.is_empty() {
                        this.ask_side(q, window, cx);
                    }
                }
            },
        );
        input.update(cx, |i, cx| i.focus(window, cx));
        self.side = Some(Side {
            input: Some(input),
            question: String::new(),
            answer: cx.new(|cx| TextViewState::markdown("", cx)),
            text: String::new(),
            done: false,
            error: None,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            agent: None,
            _reader: None,
            _sub: Some(sub),
        });
        cx.notify();
    }

    /// Replaces any earlier side question; only the latest is kept.
    fn ask_side(&mut self, question: String, window: &mut Window, cx: &mut Context<Self>) {
        let mut side = Side {
            input: None,
            question: question.clone(),
            answer: cx.new(|cx| TextViewState::markdown("", cx)),
            text: String::new(),
            done: false,
            error: None,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            agent: None,
            _reader: None,
            _sub: None,
        };
        match agent::spawn_side(&self.ws, &self.meta) {
            Ok((mut a, rx)) => match a.send(&question) {
                Ok(()) => {
                    side.agent = Some(a);
                    side._reader = Some(cx.spawn(async move |this, cx| {
                        while let Ok(ev) = rx.recv().await {
                            if this.update(cx, |c, cx| c.apply_side(ev, cx)).is_err() {
                                break;
                            }
                        }
                    }));
                }
                Err(e) => (side.done, side.error) = (true, Some(format!("send failed: {e}"))),
            },
            Err(e) => {
                (side.done, side.error) = (true, Some(format!("failed to start claude: {e}")))
            }
        }
        side.focus.focus(window, cx);
        self.side = Some(side);
        cx.notify();
    }

    fn apply_side(&mut self, ev: Ev, cx: &mut Context<Self>) {
        let Some(side) = &mut self.side else { return };
        match ev {
            Ev::TextDelta(d) => {
                side.text.push_str(&d);
                side.answer.update(cx, |m, cx| m.push_str(&d, cx));
            }
            Ev::Text(full) => {
                side.text = full;
                let t = side.text.clone();
                side.answer.update(cx, |m, cx| m.set_text(&t, cx));
            }
            Ev::Result { is_error, text, .. } => {
                side.done = true;
                side.agent = None; // stays alive on stdin otherwise
                if is_error {
                    side.error = Some(text);
                }
            }
            Ev::Exited(msg) if !side.done => (side.done, side.error) = (true, Some(msg)),
            _ => return,
        }
        side.scroll.scroll_to_bottom();
        cx.notify();
    }

    fn close_side(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.side.take().is_some() {
            self.focus_composer(window, cx);
            cx.notify();
        }
    }

    fn copy_side(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(side) = &self.side
            && !side.text.is_empty()
        {
            cx.write_to_clipboard(ClipboardItem::new_string(side.text.clone()));
            window.push_notification("Copied the answer", cx);
        }
    }

    /// Puts the answer into the composer after any draft, to send or edit.
    fn insert_side(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(side) = self.side.take() else { return };
        if !side.text.is_empty() {
            self.composer.update(cx, |c, cx| {
                let (draft, end) = (c.value().trim_end().len(), c.value().len());
                let sep = if draft == 0 { "" } else { "\n\n" };
                c.set_selected_range(draft..end, cx);
                c.replace(format!("{sep}{}", side.text), window, cx);
            });
        }
        self.focus_composer(window, cx);
        cx.notify();
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(a) = &self.agent {
            a.stop();
            self.push(Block::Info("stopping…".into()), cx);
        }
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows = vec![];
        for (ix, b) in self.blocks.iter().enumerate() {
            rows.push((ix, None));
            if let Block::Tools { calls, open: true } = b {
                rows.extend((0..calls.len()).map(|j| (ix, Some(j))));
            }
        }
        rows
    }

    /// Keeps the cursor and scroll on the newest row if the cursor was already on the last one.
    fn follow(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Self, &mut Context<Self>)) {
        let at_end = self.sel + 1 >= self.rows().len();
        f(self, cx);
        if at_end {
            self.sel = self.rows().len().saturating_sub(1);
            self.scroll.scroll_to_bottom();
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

    fn apply_inner(&mut self, ev: Ev, cx: &mut Context<Self>) {
        match ev {
            Ev::Init(id) => {
                if self.meta.session_id.as_ref() != Some(&id) {
                    self.meta.session_id = Some(id);
                    cx.emit(ChatEvent::Meta);
                }
                // Each turn starts with init, including ones the CLI starts itself when a
                // background task finishes.
                self.busy = true;
                self.steered = false;
            }
            Ev::Background(n) => self.background = n,
            Ev::User(text) => self.blocks.push(Block::User(text)),
            Ev::TextDelta(s) => match self.blocks.last_mut() {
                Some(Block::Text { md, done: false }) => {
                    md.update(cx, |m, cx| m.push_str(&s, cx));
                }
                _ => {
                    let md = cx.new(|cx| TextViewState::markdown(&s, cx));
                    self.blocks.push(Block::Text { md, done: false });
                }
            },
            Ev::Text(full) => match self.blocks.last_mut() {
                Some(Block::Text { md, done }) if !*done => {
                    md.update(cx, |m, cx| m.set_text(&full, cx));
                    *done = true;
                }
                _ => {
                    let md = cx.new(|cx| TextViewState::markdown(&full, cx));
                    self.blocks.push(Block::Text { md, done: true });
                }
            },
            Ev::ToolUse { id, name, input } => {
                let call = Call {
                    summary: summarize(&input, &self.ws.dir().to_string_lossy()),
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
                let call = self.blocks.iter_mut().rev().find_map(|b| match b {
                    Block::Tool(c) => (c.id == id).then_some(c),
                    Block::Tools { calls, .. } => calls.iter_mut().find(|c| c.id == id),
                    _ => None,
                });
                if let Some(c) = call {
                    c.result = Some(text);
                    c.is_error = is_error;
                }
            }
            Ev::Result {
                is_error,
                text,
                cost,
                secs,
            } => {
                self.busy = false;
                let took = match secs {
                    0..60 => format!("{secs}s"),
                    _ => format!("{}m {}s", secs / 60, secs % 60),
                };
                self.blocks.push(Block::Info(if is_error {
                    format!("error: {text}")
                } else {
                    format!("done · {took} · ${cost:.2}")
                }));
                cx.emit(ChatEvent::TurnDone);
            }
            Ev::Exited(msg) => {
                self.agent = None;
                self.background = 0;
                if self.busy {
                    self.busy = false;
                    self.blocks.push(Block::Info(msg));
                    cx.emit(ChatEvent::TurnDone);
                }
            }
        }
    }

    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.sel = ix.min(self.rows().len().saturating_sub(1));
        self.scroll.scroll_to_item(self.sel);
        cx.notify();
    }

    fn render_row(
        &self,
        ix: usize,
        (bi, call): Row,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
        let tool_row = |c: &Call| {
            v_flex()
                .child(
                    h_flex()
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
                        .child(status(c)),
                )
                .when(c.open, |d| {
                    let text: String = c
                        .result
                        .as_deref()
                        .unwrap_or("(running)")
                        .chars()
                        .take(20_000)
                        .collect();
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
            (Block::User(t), _) => column()
                .px_3()
                .py_2()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .bg(theme.secondary)
                .text_color(theme.secondary_foreground)
                .text_size(px(14.))
                .line_height(relative(1.5))
                .child(t.clone()),
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

    /// The side-question card, just above the composer so the transcript stays in view.
    fn render_side(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let side = self.side.as_ref()?;
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let hint = |k: &str, label: &'static str| {
            h_flex()
                .gap_1()
                .child(Kbd::new(Keystroke::parse(k).unwrap()))
                .child(label)
        };
        let focused = side.focus.is_focused(window)
            || (side.input.as_ref()).is_some_and(|i| i.focus_handle(cx).is_focused(window));
        let header = h_flex()
            .gap_2()
            .text_xs()
            .text_color(muted)
            .child(Icon::new(IconName::MessageCircleQuestionMark).with_size(px(14.)))
            .child("Side question · not added to chat")
            .child(div().flex_1())
            .when(!side.done && side.input.is_none(), |d| {
                d.child(Spinner::new().with_size(px(12.)))
            })
            .child(
                Button::new("close-side")
                    .ghost()
                    .xsmall()
                    .icon(IconName::X)
                    .tooltip("Dismiss (esc)")
                    .on_click(cx.listener(|this, _, window, cx| this.close_side(window, cx))),
            );
        let body = match &side.input {
            Some(input) => div()
                .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                    this.close_side(window, cx);
                    cx.stop_propagation();
                }))
                .child(Textarea::new(input).appearance(false).text_size(px(14.)))
                .into_any_element(),
            None => v_flex()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child(side.question.clone()),
                )
                .child(
                    div()
                        .id("side-answer")
                        .key_context("SideAnswer")
                        .track_focus(&side.focus)
                        .max_h(px(280.))
                        .overflow_y_scroll()
                        .track_scroll(&side.scroll)
                        .text_size(px(14.))
                        .line_height(relative(1.5))
                        .on_action(cx.listener(|this, _: &CloseSide, window, cx| {
                            this.close_side(window, cx)
                        }))
                        .on_action(
                            cx.listener(|this, _: &CopySide, window, cx| {
                                this.copy_side(window, cx)
                            }),
                        )
                        .on_action(cx.listener(|this, _: &InsertSide, window, cx| {
                            this.insert_side(window, cx)
                        }))
                        .child(TextView::new(&side.answer).selectable(true))
                        .when_some(side.error.clone(), |d, e| {
                            d.child(div().text_xs().text_color(theme.danger_foreground).child(e))
                        }),
                )
                .child(
                    h_flex()
                        .gap_3()
                        .text_xs()
                        .text_color(muted)
                        .child(hint("escape", "dismiss"))
                        .child(hint("c", "copy"))
                        .child(hint("enter", "insert into message")),
                )
                .into_any_element(),
        };
        Some(
            v_flex()
                .w_full()
                .max_w(px(712.))
                .mx_auto()
                .mb_2()
                .p_3()
                .gap_2()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(if focused { theme.primary } else { theme.border })
                .bg(theme.muted)
                .child(header)
                .child(body)
                .into_any_element(),
        )
    }

    /// Opens upward over the transcript, like Slack's: the composer sits at the bottom.
    fn render_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let m = self.menu.as_ref()?;
        let theme = cx.theme();
        let faint = crate::app::faint(theme);
        let (fg, muted, active, hover) = (
            theme.foreground,
            theme.muted_foreground,
            theme.list_active,
            theme.list_hover,
        );
        let multi = self.ws.repos.len() > 1;
        let rows: Vec<AnyElement> = (0..m.matches.len())
            .filter_map(|ix| {
                let it = self.menu_item(ix)?;
                let icon = match it.kind {
                    Kind::File => IconName::File,
                    Kind::Dir => IconName::Folder,
                    Kind::Command => IconName::SquareSlash,
                };
                let badge = match it.kind {
                    Kind::Command => (!it.badge.is_empty())
                        .then(|| div().flex_none().text_color(faint).child(it.badge.clone())),
                    // The repo, so two README.md rows are told apart at a glance.
                    _ => multi.then(|| {
                        h_flex()
                            .flex_none()
                            .gap_1()
                            .text_color(muted)
                            .child(Icon::new(IconName::FolderGit2).with_size(px(12.)))
                            .child(it.badge.clone())
                    }),
                };
                Some(
                    h_flex()
                        .id(ix)
                        .h(px(28.))
                        .px_2()
                        .gap_2()
                        .rounded(theme.radius)
                        .text_sm()
                        .when(ix == m.sel, |d| d.bg(active))
                        .when(ix != m.sel, |d| d.hover(|d| d.bg(hover)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                this.pick(ix, window, cx);
                                window.prevent_default(); // keep focus in the composer
                            }),
                        )
                        .child(Icon::new(icon).with_size(px(14.)).text_color(muted))
                        .child(div().flex_none().text_color(fg).child(it.label.clone()))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(muted)
                                .child(it.detail.clone()),
                        )
                        .children(badge.map(|b| b.text_xs()))
                        .into_any_element(),
                )
            })
            .collect();
        let empty = match (m.command, rows.is_empty()) {
            (_, false) => None,
            (true, _) if self.probing.is_some() && self.commands.is_empty() => {
                Some("Loading skills…")
            }
            (false, _) if self.indexing.is_some() && self.files.is_empty() => {
                Some("Indexing files…")
            }
            (true, _) => Some("No matching skills or commands"),
            (false, _) => Some("No matching files"),
        };
        let hint = |k: &str, label: &'static str| {
            h_flex()
                .gap_1()
                .children(k.split(' ').map(|k| Kbd::new(Keystroke::parse(k).unwrap())))
                .child(label)
        };
        Some(
            v_flex()
                .id("completions")
                .absolute()
                .left_0()
                .right_0()
                .bottom_full()
                .mb_1()
                .p_1()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .bg(theme.secondary)
                .occlude()
                .child(
                    div()
                        .id("completion-rows")
                        .max_h(px(8. * 28.))
                        .overflow_y_scroll()
                        .track_scroll(&self.menu_scroll)
                        .children(rows)
                        .when_some(empty, |d, e| {
                            d.child(
                                div()
                                    .h(px(28.))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .text_sm()
                                    .text_color(muted)
                                    .child(e),
                            )
                        }),
                )
                .child(
                    h_flex()
                        .px_2()
                        .pt_1()
                        .gap_3()
                        .text_xs()
                        .text_color(faint)
                        .child(hint("up down", "navigate"))
                        .child(hint("enter", "add"))
                        .child(hint("escape", "close"))
                        .when(!m.command && multi, |d| {
                            d.child(div().flex_1())
                                .child("Type repo/ to search one repo")
                        }),
                )
                .into_any_element(),
        )
    }

    /// Queued messages above the input, in send order: the top row goes next.
    fn render_queue(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.queue.is_empty() {
            return None;
        }
        let focused = self.qfocus.is_focused(window);
        let theme = cx.theme();
        let faint = crate::app::faint(theme);
        let (fg, muted, active, hover, primary, transparent, radius, border) = (
            theme.foreground,
            theme.muted_foreground,
            theme.list_active,
            theme.list_hover,
            theme.primary,
            theme.transparent,
            theme.radius,
            theme.border,
        );
        let action = |id: (&'static str, usize), icon: IconName, tip: &'static str| {
            Button::new(id).ghost().xsmall().icon(icon).tooltip(tip)
        };
        let rows: Vec<AnyElement> = (self.queue.iter().enumerate())
            .map(|(ix, q)| {
                let sel = focused && ix == self.qsel;
                h_flex()
                    .id(("queued", ix))
                    .h(px(28.))
                    .pl_2()
                    .gap_2()
                    .rounded(radius)
                    .border_l_2()
                    .border_color(if sel { primary } else { transparent })
                    .when(sel, |d| d.bg(active))
                    .when(!sel, |d| d.hover(|d| d.bg(hover)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.qsel = ix;
                        this.focus_queue(window, cx);
                    }))
                    .child(
                        Icon::new(IconName::CornerDownRight)
                            .with_size(px(14.))
                            .text_color(muted),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(fg)
                            .child(q.preview.clone()),
                    )
                    .child(
                        action(("q-send", ix), IconName::ArrowUp, "Send now (enter)").on_click(
                            cx.listener(move |this, _, window, cx| {
                                this.qsel = ix;
                                this.steer(window, cx);
                                cx.stop_propagation(); // the row's own click would refocus the queue
                            }),
                        ),
                    )
                    .child(
                        action(("q-edit", ix), IconName::Pencil, "Edit (e)").on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.qsel = ix;
                                this.edit_queued(window, cx);
                                cx.stop_propagation(); // the row's own click would refocus the queue
                            },
                        )),
                    )
                    .child(
                        action(("q-del", ix), IconName::Trash, "Delete (d)").on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.qsel = ix;
                                this.delete_queued(window, cx);
                                cx.stop_propagation(); // the row's own click would refocus the queue
                            },
                        )),
                    )
                    .into_any_element()
            })
            .collect();
        let hint = |k: &str, label: &'static str| {
            h_flex()
                .gap_1()
                .children(k.split(' ').map(|k| Kbd::new(Keystroke::parse(k).unwrap())))
                .child(label)
        };
        let n = self.queue.len();
        Some(
            v_flex()
                .id("queue")
                .key_context("Queue")
                .track_focus(&self.qfocus)
                .pb_1()
                .mb_1()
                .border_b_1()
                .border_color(border)
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
                        .text_color(muted)
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
        let Some(&(bi, call)) = self.rows().get(self.sel) else {
            return;
        };
        match (&mut self.blocks[bi], call) {
            (Block::Tool(c), _) => c.open = !c.open,
            (Block::Tools { calls, .. }, Some(j)) => calls[j].open = !calls[j].open,
            (Block::Tools { open, .. }, None) => *open = !*open,
            _ => return,
        }
        cx.notify();
    }
}

impl Render for Chat {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The queue emptied (a turn ended, a git step finished) while it had focus.
        if self.queue.is_empty() && self.qfocus.is_focused(window) {
            self.focus_composer(window, cx);
        }
        let focused = self.focus.is_focused(window);
        let theme = cx.theme();
        let (radius, border, card, muted) = (
            theme.radius_lg,
            theme.border,
            theme.secondary,
            theme.muted_foreground,
        );
        let blocks: Vec<AnyElement> = (self.rows().into_iter().enumerate())
            .map(|(ix, row)| self.render_row(ix, row, focused, cx))
            .collect();
        v_flex()
            .size_full()
            .child(
                div()
                    .id("transcript")
                    .key_context("Transcript")
                    .track_focus(&self.focus)
                    .flex_1()
                    .min_h_0()
                    .py_2()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .vertical_scrollbar(&self.scroll)
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
                    .children(blocks),
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
                                if let Some(m) = &this.menu
                                    && !a.shift
                                    && !m.matches.is_empty()
                                {
                                    this.pick(m.sel, window, cx);
                                    cx.stop_propagation();
                                }
                            }))
                            .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                                if let Some(m) = &this.menu
                                    && !m.matches.is_empty()
                                {
                                    this.pick(m.sel, window, cx);
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
                                            .tooltip_with_action("Change model", &PickModel, None)
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
                                    .child(Kbd::new(Keystroke::parse("cmd-shift-enter").unwrap()))
                                    .child("side question"),
                            ),
                    ),
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
            result: Some(String::new()),
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
    }
}

#[cfg(all(test, feature = "test-support"))]
mod ui_tests {
    use super::*;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin
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

    /// Every test that spawns claude installs the same script, so racing on the variable is fine.
    fn fake_claude(root: &std::path::Path) -> std::path::PathBuf {
        let fake = root.join("claude");
        std::fs::write(&fake, FAKE).unwrap();
        std::process::Command::new("chmod")
            .args(["+x"])
            .arg(&fake)
            .status()
            .unwrap();
        // SAFETY: tests only ever set it to this same script.
        unsafe { std::env::set_var("SHARK_CLAUDE", &fake) };
        fake
    }

    /// A side question forks the chat's session with no tools and nothing saved.
    #[test]
    fn side_question_process() {
        let root = std::env::temp_dir().join(format!("shark-side-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let fake = fake_claude(&root);
        let ws = Workspace {
            name: format!("side-proc-{}", std::process::id()),
            ..Default::default()
        };
        let meta = ChatMeta {
            id: ws.name.clone(),
            session_id: Some("s1".into()),
            ..Default::default()
        };
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
        fake_claude(&root);
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
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                c.apply_side(Ev::TextDelta("Forty".into()), cx);
                c.apply_side(Ev::Text("Forty-two".into()), cx);
                c.apply_side(
                    Ev::Result {
                        is_error: false,
                        text: String::new(),
                        cost: 0.,
                        secs: 0,
                    },
                    cx,
                );
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
        cx.update(|cx| chat.update(cx, |c, cx| c.select(0, cx)));
        frame(cx);
        let scroll = cx.update(|cx| chat.read(cx).scroll.clone());
        assert!(scroll.max_offset().y > px(0.), "transcript must overflow");
        assert_eq!(scroll.offset().y, px(0.), "scrolled to the top");

        cx.update(|cx| chat.update(cx, |c, cx| c.sent("new".into(), cx)));
        frame(cx);
        assert_eq!(scroll.offset().y, -scroll.max_offset().y, "at the bottom");
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
        fake_claude(&root);
        let ws = Workspace {
            name: format!("queue-test-{}", std::process::id()),
            branch: "b".into(),
            repos: vec![],
            ..Default::default()
        };
        let meta = ChatMeta {
            id: ws.name.clone(),
            ..Default::default()
        };
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
                        Block::User(t) => Some(t.clone()),
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
