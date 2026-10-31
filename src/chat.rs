//! One agent conversation (a chat tab): transcript blocks, composer, and the claude process.
mod completion;
#[cfg(all(test, feature = "test-support"))]
pub(crate) mod completion_pixels;
mod critters;
mod links;
mod model;
mod planmode;
mod question;
mod queue;
mod review;
mod side;
mod tools;

use self::queue::Queued;
use crate::agent::{self, Agent, Ev};
use crate::complete::{self, Item};
use crate::models::EFFORT;
use crate::plan::{self, PlanPane};
use crate::state::{ChatMeta, Workspace};
use crate::ui::{hint, plural};
use crate::vim::{self, Vim};
use crate::*;
use completion::Menu;
use gpui_kit::assets::IconName;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::hover_card::HoverCard;
use gpui_kit::component::input::{
    Enter, Escape, IndentInline, InputContent, InputEvent, MoveDown, MoveUp, OutdentInline, Redo,
    Textarea, TextareaState, Undo,
};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::shimmer::ShimmerText;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::{TextView, TextViewState, TextViewStyle};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme, Icon, Selectable as _, ThemeColor, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
pub(crate) use links::{copy_button, link_roots, markdown};
#[cfg(all(test, feature = "test-support"))]
pub(crate) use model::model_entries;
pub(crate) use model::{effort_label, model_label};
use planmode::{ENTER_PLAN, NEEDS_APPROVAL, PLAN_SHOWN};
pub(crate) use review::{ReviewCard, ReviewItem, parse_reports};
use review::{card_of, hidden_reply, hide_reports, may_report, review_title, split_notes};
use serde_json::Value;
use side::Side;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;
use tools::{ASK_TOOL, Call, PLAN_TOOL, full_input, group_summary, standalone, summarize};

enum Block {
    /// `true` while queued: sent mid-turn, and claude hasn't read it yet.
    User(String, bool),
    Text {
        md: Entity<TextViewState>,
        /// The markdown source, for copying: the view doesn't hand it back.
        src: String,
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
    /// A diff review (`send_review`); `true` while claude hasn't read it, like `User`.
    Review(Rc<ReviewCard>, bool),
}

impl Block {
    /// A message's text (a review's prompt) and whether it is still queued; `None` for the rest.
    fn message(&self) -> Option<(&str, bool)> {
        match self {
            Block::User(t, queued) => Some((t, *queued)),
            Block::Review(r, queued) => Some((&r.prompt, *queued)),
            _ => None,
        }
    }
}

/// A transcript row: a block, or one call inside an expanded tool group.
type Row = (usize, Option<usize>);

pub enum ChatEvent {
    /// `meta` changed (session, model or title); the app saves it.
    Meta,
    /// The user picked the model or thinking level (after `Meta`); new chats start on it.
    Picked,
    TurnDone,
    GitReviewsSent(Vec<String>),
    /// The agent started or stopped waiting on answers to its questions (`Chat::asking`).
    Asking,
    /// A review card's "Review changes" row: show the diff.
    ShowChanges,
    /// A review card's comment link: show that comment in the diff.
    ShowComment(u64),
    /// Remove on a queued review card: those comments never went out.
    ReviewRemoved(Vec<u64>),
    /// A review went out (now, or from the queue): its comments are read in `turn`.
    ReviewSent {
        ids: Vec<u64>,
        turn: usize,
    },
}

pub struct Chat {
    pub ws: Workspace,
    pub meta: ChatMeta,
    blocks: Vec<Block>,
    /// What the list shows, derived from `blocks`; see `sync_rows`.
    rows: Vec<Row>,
    sel: usize,
    pub busy: bool,
    /// Background tasks (subagents, shells) still running; the turn can end before they do.
    pub background: Vec<agent::Task>,
    critters: HashMap<String, usize>,
    hop: usize,
    hopping: Option<Task<()>>,
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
    /// The reply of the turn that ended last, and that turn's number (see `turns`).
    pub last_reply: String,
    pub last_turn: usize,
    /// Vim keys in the composer (`State::vim`); it starts each message in Insert mode.
    pub vim: Vim,
    pub vim_on: bool,
    /// The key the interceptor just handled; its key-down still arrives and must not run twice.
    vim_took: Option<Keystroke>,
    /// The model menu, opening upward from the model button, whose bounds the last paint kept.
    model_menu: Option<(Entity<crate::menu::Menu>, Subscription)>,
    model_at: std::rc::Rc<std::cell::Cell<Bounds<Pixels>>>,
    /// Model and (claude) thinking level the running process was started with; a mismatch
    /// respawns it on the next send.
    agent_model: Option<String>,
    agent_effort: &'static str,
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
    /// PRs of every repo, as last listed; cached on disk per source repository.
    prs: Vec<complete::Pr>,
    pr_items: Vec<Item>,
    fetching_prs: Option<Task<()>>,
    /// Repos whose PR list hasn't landed yet in the running fetch.
    prs_left: usize,
    /// `(repo, reason)` for each repo the last fetch couldn't list.
    prs_errs: Vec<(String, String)>,
    /// When the last fetch that listed every repo finished.
    prs_at: Option<Instant>,
    /// User blocks claude hasn't echoed yet, oldest first.
    unread: VecDeque<usize>,
    /// The turn was interrupted; its result reads "interrupted".
    cut: bool,
    /// `total_cost_usd` of the last result: claude reports a running total, shown per turn.
    cost_total: f64,
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
    editing_git_reviews: Vec<String>,
    /// A message was sent into a running turn and claude may not have read it yet.
    pub steered: bool,
    /// What the CLI reported at init: its model, permission mode and MCP servers.
    reported: Option<String>,
    /// codex's config puts threads on the Fast tier; `meta.fast` overrides it.
    fast_by_config: bool,
    side: Option<Side>,
    /// Questions the agent waits on, in a card above the composer.
    asked: Option<question::Asked>,
    completed_answers: std::collections::BTreeMap<String, [String; 2]>,
    pub(crate) history_save_error: Option<String>,
    /// The question card had the focus at the last render: when it goes, the composer gets it.
    asked_focused: bool,
    /// Plan versions and the pane that shows them, next to the transcript while `plan_open`.
    pub plan: Entity<PlanPane>,
    pub plan_open: bool,
    /// Files from Finder being dragged over the pane.
    dropping: Option<Vec<PathBuf>>,
    _subs: Vec<Subscription>,
}

/// How long an interrupt may take before the process is stopped instead.
const INTERRUPT_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

impl EventEmitter<ChatEvent> for Chat {}

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

/// Feeds a process's events to `apply` until it ends or `T` is gone.
pub(crate) fn read_events<T: 'static>(
    rx: gpui_kit::base::async_util::Receiver<Ev>,
    apply: fn(&mut T, Ev, &mut Context<T>),
    cx: &mut Context<T>,
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        while let Ok(ev) = rx.recv().await {
            if this.update(cx, |t, cx| apply(t, ev, cx)).is_err() {
                break;
            }
        }
    })
}

/// A turn's cost from claude's running `total` (updated here); a lower total is a fresh
/// process's, shown as it is.
fn turn_cost(total: &mut f64, cost: f64) -> f64 {
    let turn = if cost >= *total { cost - *total } else { cost };
    if cost > 0. {
        *total = cost; // codex and failures report none
    }
    turn
}

/// "48s", "2m 5s".
pub(crate) fn duration(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        _ => format!("{}m {}s", secs / 60, secs % 60),
    }
}

impl Chat {
    pub fn new(
        ws: Workspace,
        meta: ChatMeta,
        vim_on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 10)
                .placeholder("Message the agent (@ to add files, / for skills)")
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
        // Runs before key bindings, so Normal-mode keys never reach the input or the keymap.
        let this = cx.entity().downgrade();
        let _keys = cx.intercept_keystrokes(move |ev, window, cx| {
            let Some(this) = this.upgrade() else { return };
            // The second key of an app chord (`ctrl-w h`) belongs to the keymap.
            if !window.has_pending_keystrokes()
                && this.update(cx, |c, cx| {
                    let took = c.vim_intercept(&ev.keystroke, window, cx);
                    c.vim_took = took.then(|| ev.keystroke.clone());
                    took
                })
            {
                cx.stop_propagation();
            }
        });
        let ws_name = ws.name.clone();
        let prs = ws
            .repos
            .iter()
            .flat_map(|r| complete::cached_repo_prs(&r.name, &r.src, false))
            .collect::<Vec<_>>();
        let plan = cx.new(|cx| PlanPane::new(&meta.id, cx));
        let plan_open = meta.plan;
        let mut chat = Chat {
            ws,
            meta,
            blocks: vec![],
            rows: vec![],
            sel: 0,
            busy: false,
            background: vec![],
            critters: HashMap::new(),
            hop: 0,
            hopping: None,
            started: None,
            hold: false,
            archiving: false,
            note: None,
            agent: None,
            composer,
            last_reply: String::new(),
            last_turn: 0,
            vim: insert_mode(),
            vim_on,
            vim_took: None,
            model_menu: None,
            model_at: Default::default(),
            agent_model: None,
            agent_effort: EFFORT,
            list: ListState::new(1, ListAlignment::Top, px(400.)), // the activity footer
            focus: cx.focus_handle(),
            reader: None,
            commands: complete::cached_commands(&ws_name),
            files: vec![],
            menu: None,
            menu_scroll: ScrollHandle::new(),
            indexing: None,
            probing: None,
            pr_items: complete::pr_items(&prs),
            prs,
            fetching_prs: None,
            prs_left: 0,
            prs_errs: vec![],
            prs_at: None,
            unread: VecDeque::new(),
            cut: false,
            cost_total: 0.,
            stopping: None,
            replaying: false,
            queue: vec![],
            qsel: 0,
            qfocus: cx.focus_handle(),
            qscroll: ScrollHandle::new(),
            editing: None,
            editing_git_reviews: vec![],
            steered: false,
            side: None,
            asked: None,
            completed_answers: Default::default(),
            history_save_error: None,
            asked_focused: false,
            reported: None,
            fast_by_config: codex::fast_by_config(),
            plan_open,
            dropping: None,
            plan,
            _subs: vec![_sub, _keys],
        };
        // Warm both sources now so the first `@` or `/` doesn't wait.
        chat.reindex(cx);
        chat.reprobe(cx);
        chat.list.set_follow_mode(FollowMode::Tail);
        // Read history off the UI thread, up to its current end: a process started meanwhile
        // appends to the same file, and its lines arrive live.
        let path = crate::transcript::transcript_path(&chat.meta.id);
        let len = std::fs::metadata(&path).map_or(0, |m| m.len());
        cx.spawn(async move |this, cx| {
            let evs = (cx.background_executor())
                .spawn(async move { crate::transcript::read_history(&path, len) })
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
            std::mem::take(&mut self.critters),
            self.hop,
            self.hopping.take(),
            self.started,
            self.cut,
            self.meta.session_id.clone(),
        );
        (self.busy, self.cut, self.replaying) = (false, false, true);
        self.cost_total = 0.;
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
            self.critters,
            self.hop,
            self.hopping,
            self.started,
            self.cut,
            self.meta.session_id,
        ) = kept;
        let n = self.blocks.len();
        self.blocks.extend(live);
        self.unread = unread.into_iter().map(|i| i + n).collect();
        self.rows = self.build_rows();
        self.list.reset(self.rows.len() + 1); // + the activity footer
        self.list.set_follow_mode(FollowMode::Tail);
        self.sel = self.rows.len().saturating_sub(1);
        let first = (self.blocks.iter()).find_map(|b| match b {
            Block::User(t, _) => Some(title_of(t)),
            Block::Review(r, _) => Some(review_title(r)),
            _ => None,
        });
        if self.meta.title.is_empty()
            && let Some(t) = first
        {
            self.meta.title = t;
            cx.emit(ChatEvent::Meta);
        }
        cx.notify();
    }

    /// Whether anything is still going on: a turn, or tasks it left running in the background.
    pub fn working(&self) -> bool {
        self.busy || !self.background.is_empty()
    }

    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// The tab label.
    pub fn title(&self) -> SharedString {
        match self.meta.title.is_empty() {
            true => "New chat".into(),
            false => self.meta.title.clone().into(),
        }
    }

    pub fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |c, cx| c.focus(window, cx));
        self.reset_vim(cx); // `i` from the transcript or queue means type
    }

    /// Vim keys in the composer on or off; either way it is back in Insert mode.
    pub fn set_vim(&mut self, on: bool, cx: &mut Context<Self>) {
        self.vim_on = on;
        self.reset_vim(cx);
    }

    fn reset_vim(&mut self, cx: &mut Context<Self>) {
        self.vim = insert_mode();
        cx.notify();
    }

    /// A composer key for Vim; true when handled. Esc closes an open menu first; in idle
    /// Normal mode Esc goes to the keymap (FocusTranscript) and Enter to the input (send).
    fn vim_intercept(
        &mut self,
        ks: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.vim_on
            || self.menu.is_some()
            || !self.composer.read(cx).focus_handle(cx).is_focused(window)
        {
            return false;
        }
        let idle = self.vim.idle();
        // ctrl-c stops the agent here, and shift-tab toggles plan mode.
        let app_key = (ks.key == "c" && ks.modifiers.control)
            || ks.key == "tab"
            || (idle && matches!(ks.key.as_str(), "escape" | "enter"));
        let Some(key) = (!app_key).then(|| editor::vim_key(ks)).flatten() else {
            return false;
        };
        let Some((_, cmd)) =
            editor::apply_vim_key(&mut self.vim, &self.composer, &key, false, window, cx)
        else {
            return false;
        };
        // Only undo and redo mean something in a draft.
        match cmd {
            Some(vim::Cmd::Undo) => window.dispatch_action(Box::new(Undo), cx),
            Some(vim::Cmd::Redo) => window.dispatch_action(Box::new(Redo), cx),
            _ => {}
        }
        // Completions belong to Insert: back in it, the query under the cursor reopens them.
        self.update_menu(cx);
        cx.notify();
        true
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
                self.editing_git_reviews.clear();
                self.drain(cx);
                cx.notify();
            }
            return;
        }
        // A message instead of answers: the agent ends its turn, and the message follows.
        // (Not a queued message saved after an edit: that was written before the question.)
        if self.editing.is_none() {
            self.dismiss_asked(cx);
        }
        // A review left queued by a stopped or failed turn goes out first, as its card says.
        if !self.busy && self.queue.first().is_some_and(|q| q.review.is_some()) {
            self.drain(cx);
        }
        let busy = self.busy;
        let sent = !self.hold && (!busy || now) && self.send_text(text, now, cx);
        self.steered |= sent && busy;
        if !sent && (self.busy || self.hold) {
            self.qsel = self.queue_draft(content);
        } else if !sent {
            return; // spawn or write failed; the error is in the transcript, keep the draft
        }
        if sent {
            cx.emit(ChatEvent::GitReviewsSent(std::mem::take(
                &mut self.editing_git_reviews,
            )));
        }
        self.finish_draft(window, cx);
        self.reset_vim(cx);
        if sent {
            self.drain(cx); // an edit that went straight out lets the rest follow
        }
        cx.notify();
    }

    /// What went out, oldest first: messages, and reviews as their prompt.
    #[cfg(all(test, feature = "test-support"))]
    pub(crate) fn sent_texts(&self) -> Vec<String> {
        (self.blocks.iter())
            .filter_map(|b| b.message().map(|(t, _)| t.to_string()))
            .collect()
    }

    /// Sends a host-written prompt together with any draft instructions.
    pub fn ask(&mut self, prompt: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let mut draft = self.composer.read(cx).value().trim().to_string();
        // The same prompt put back below after a failed start: retrying must not send it twice.
        if draft == prompt.trim() {
            draft.clear();
        }
        let text = match draft.is_empty() {
            true => prompt.to_string(),
            false => format!("{prompt}\n\nAlso: {draft}"),
        };
        let sent = self.send_text(text.clone(), false, cx);
        if sent && !draft.is_empty() {
            cx.emit(ChatEvent::GitReviewsSent(std::mem::take(
                &mut self.editing_git_reviews,
            )));
            self.finish_draft(window, cx);
        }
        // The agent didn't start (refusals keep `hold`/`archiving` set): a new chat's first
        // message, e.g. from New workspace, goes back to the composer instead of being lost.
        let failed = !sent && self.agent.is_none() && !self.hold && !self.archiving;
        if failed && draft.is_empty() && self.meta.session_id.is_none() {
            self.composer
                .update(cx, |c, cx| c.set_value(text, window, cx));
        }
        sent
    }

    /// Returns whether the text went out; while a turn runs only if `steer`. Mid-turn, claude
    /// holds it and reads it at its next step.
    fn send_text(&mut self, text: String, steer: bool, cx: &mut Context<Self>) -> bool {
        self.send_block(text, steer, None, cx)
    }

    /// `send_text`, shown as `review`'s card when there is one.
    fn send_block(
        &mut self,
        text: String,
        steer: bool,
        review: Option<Rc<ReviewCard>>,
        cx: &mut Context<Self>,
    ) -> bool {
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
        let stale = self.agent_model != self.meta.model
            || (!self.codex() && self.agent_effort != self.effort());
        if self.agent.is_some() && stale && !self.busy && !self.steered {
            self.agent = None; // drop stops it; the respawn resumes the same session on the new model/level
            self.set_background(vec![], cx); // its reader is replaced below, so its Exited never arrives
        }
        if self.agent.is_none() {
            match agent::spawn(&self.ws, &self.meta) {
                Ok((mut a, rx)) => {
                    if self.meta.plan
                        && let Err(e) = a.set_plan_mode(true)
                    {
                        self.push(Block::Info(format!("plan mode failed: {e}")), cx);
                        return false;
                    }
                    self.agent = Some(a);
                    self.agent_model = self.meta.model.clone();
                    self.agent_effort = self.effort();
                    self.reader = Some(read_events(rx, Self::apply, cx));
                }
                Err(e) => {
                    let codex = self.meta.is_codex();
                    let prog = if codex { "codex" } else { "claude" };
                    let why = crate::proc::spawn_err(&self.ws.dir(), prog, e);
                    let msg = format!("Couldn't start the agent: {why}. See Home for setup.");
                    self.push(Block::Info(msg), cx);
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
        let ids = review.as_ref().map(|r| r.ids());
        self.sent(text, review, cx);
        if let Some(ids) = ids {
            let turn = self.turns();
            cx.emit(ChatEvent::ReviewSent { ids, turn });
        }
        true
    }

    /// The ended turn's reply and number, for review reports: the text after the last message
    /// claude read (ones queued mid-turn and still unread don't start it).
    fn keep_reply(&mut self) {
        let start = (self.blocks.iter()).rposition(|b| matches!(b.message(), Some((_, false))));
        let after = start.map_or(0, |i| i + 1);
        self.last_reply = (self.blocks[after..].iter())
            .filter_map(|b| match b {
                Block::Text { src, .. } => Some(src.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        self.last_turn = (self.blocks[..after].iter())
            .filter(|b| b.message().is_some())
            .count();
    }

    /// Messages in the transcript; the nth one starts turn n.
    pub fn turns(&self) -> usize {
        (self.blocks.iter())
            .filter(|b| b.message().is_some())
            .count()
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
        self.prs_at = None; // the next `#` lists the new repo's PRs
    }

    fn sent(&mut self, text: String, review: Option<Rc<ReviewCard>>, cx: &mut Context<Self>) {
        let queued = self.busy;
        if !queued {
            self.started = Some(Instant::now());
        }
        self.busy = true;
        if self.meta.title.is_empty() {
            self.meta.title = match &review {
                Some(r) => review_title(r),
                None => title_of(&text),
            };
            cx.emit(ChatEvent::Meta);
        }
        // Jump to the end so push follows the new message and the reply.
        self.sel = self.rows.len().saturating_sub(1);
        self.list.set_follow_mode(FollowMode::Tail);
        self.unread.push_back(self.blocks.len());
        self.push(
            match review {
                Some(r) => Block::Review(r, queued),
                None => Block::User(text, queued),
            },
            cx,
        );
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

    /// Messages claude never read, once its process is gone; reviews among them say so.
    fn drop_unread(&mut self, cx: &mut Context<Self>) {
        let n = self.unread.len();
        for bi in std::mem::take(&mut self.unread) {
            match self.blocks.get_mut(bi) {
                Some(Block::User(_, queued)) => *queued = false,
                Some(Block::Review(r, queued)) => {
                    *queued = false;
                    cx.emit(ChatEvent::ReviewRemoved(r.ids()));
                }
                _ => {}
            }
        }
        if n > 0 {
            self.blocks.push(Block::Info(format!(
                "{n} message{} not delivered; send again",
                plural(n)
            )));
        }
    }

    /// The process is gone (or going): forget its state, end its calls, flag what it never read.
    fn drop_process(&mut self, cx: &mut Context<Self>) {
        self.agent = None; // drop stops it
        self.drop_asked(cx);
        self.set_background(vec![], cx);
        self.stopping = None;
        self.cut = false;
        self.steered = false;
        self.interrupt();
        self.drop_unread(cx);
    }

    /// Forgets the claude session and stops the agent; the next send starts a new conversation.
    pub fn new_conversation(&mut self, cx: &mut Context<Self>) {
        let was_working = self.working();
        self.meta.session_id = None;
        cx.emit(ChatEvent::Meta);
        self.reader = None; // its Exited would be about the old process
        self.busy = false;
        self.follow(cx, |this, cx| {
            this.drop_process(cx);
            this.blocks.push(Block::Info("new conversation".into()));
        });
        if was_working {
            self.keep_reply();
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
        let ok = !self.cut
            && matches!(
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
        let bi = (self.blocks.iter()).rposition(|b| !matches!(b.message(), Some((_, true))))?;
        matches!(self.blocks[bi], Block::Text { done: false, .. }).then_some(bi)
    }

    fn apply_inner(&mut self, ev: Ev, cx: &mut Context<Self>) {
        match ev {
            Ev::Init {
                session_id,
                model,
                permission_mode,
                mcp,
            } => self.on_init(session_id, &model, &permission_mode, &mcp, cx),
            Ev::Background(tasks) => self.set_background(tasks, cx),
            Ev::Interrupted => self.cut = true,
            Ev::User(text) => self.on_echo(text),
            Ev::Notice(text) => self.blocks.push(Block::Info(text)),
            Ev::TextDelta(s) => self.on_text(s, cx),
            Ev::Text(full) => self.on_full_text(full, cx),
            Ev::ToolUse { id, name, input } => self.on_tool_use(id, name, input),
            Ev::ToolResult { id, text, is_error } => self.on_tool_result(id, text, is_error),
            Ev::Permission {
                request_id,
                tool,
                tool_use_id,
                input,
            } => self.on_permission(request_id, tool, tool_use_id, input, cx),
            // codex's turn goes on to its end by itself: there is no request to answer. The
            // pane keeps its versions, so history doesn't add them again.
            Ev::Plan { id, text } => {
                let row =
                    (self.blocks.iter()).rposition(|b| matches!(b, Block::Tool(c) if c.id == id));
                if let Some(bi) = row
                    && let Block::Tool(c) = &mut self.blocks[bi]
                {
                    c.summary = plan::title(&text); // its row started out empty
                    self.remeasure(bi);
                }
                if !self.replaying {
                    self.show_plan(id, text, cx);
                }
            }
            Ev::Result {
                is_error,
                text,
                cost,
                secs,
            } => self.on_result(is_error, text, cost, secs, cx),
            Ev::Exited(msg) => {
                self.drop_process(cx);
                if self.busy {
                    self.busy = false;
                    self.blocks.push(Block::Info(msg));
                    self.keep_reply();
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

    /// The CLI's init, which starts each turn: its session, and what it reported about itself.
    fn on_init(
        &mut self,
        id: String,
        model: &str,
        permission_mode: &str,
        mcp: &[String],
        cx: &mut Context<Self>,
    ) {
        // A process of the backend the chat just switched away from: its session
        // can't be resumed by the new one.
        let stale =
            !self.replaying && codex::is_codex(self.agent_model.as_deref()) != self.meta.is_codex();
        if !stale && self.meta.session_id.as_ref() != Some(&id) {
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

    /// claude's echo: the oldest message it hadn't read is now part of the turn.
    /// `ends_with`: a workspace `note` may have been put before the text on the wire.
    /// Not just the oldest: one the CLI refused (a failed codex turn/start) is never echoed.
    fn on_echo(&mut self, text: String) {
        let read = (self.unread.iter())
            .position(|&bi| (self.blocks[bi].message()).is_some_and(|(t, _)| text.ends_with(t)));
        match read {
            Some(k) => {
                let bi = self.unread.remove(k).unwrap();
                if let Block::User(_, queued) | Block::Review(_, queued) = &mut self.blocks[bi] {
                    *queued = false;
                }
                self.remeasure(bi);
                // codex reads a steer into the running turn, with no new init after.
                if self.unread.is_empty() {
                    self.steered = false;
                }
            }
            None => {
                // A replayed message carries the workspace notes sent with it.
                let (notes, text) = split_notes(&text);
                self.blocks
                    .extend(notes.lines().map(|n| Block::Info(n.into())));
                self.blocks.push(match card_of(text) {
                    Some(card) => Block::Review(Rc::new(card), false),
                    None => Block::User(text.into(), false),
                });
            }
        }
    }

    fn on_text(&mut self, s: String, cx: &mut Context<Self>) {
        match self.live_text() {
            Some(bi) => {
                if let Block::Text { md, src, .. } = &mut self.blocks[bi] {
                    // Reports come last. A line that may start one waits until it ends,
                    // then the text is rebuilt without them: once per such line, not per
                    // delta. Only its lines are checked, so a long reply stays linear.
                    let from = src.rfind('\n').map_or(0, |i| i + 1);
                    src.push_str(&s);
                    match may_report(&src[from..]) {
                        true if s.contains('\n') => {
                            md.update(cx, |m, cx| m.set_text(&hide_reports(src), cx))
                        }
                        true => {}
                        false => md.update(cx, |m, cx| m.push_str(&s, cx)),
                    }
                }
                self.remeasure(bi);
            }
            None => {
                let md = cx.new(|cx| TextViewState::markdown(&hide_reports(&s), cx));
                self.blocks.push(Block::Text {
                    md,
                    src: s,
                    done: false,
                });
            }
        }
    }

    fn on_full_text(&mut self, full: String, cx: &mut Context<Self>) {
        match self.live_text() {
            Some(bi) => {
                if let Block::Text { md, src, done } = &mut self.blocks[bi] {
                    md.update(cx, |m, cx| m.set_text(&hide_reports(&full), cx));
                    *src = full;
                    *done = true;
                }
                self.remeasure(bi);
            }
            None => {
                let md = cx.new(|cx| TextViewState::markdown(&hide_reports(&full), cx));
                self.blocks.push(Block::Text {
                    md,
                    src: full,
                    done: true,
                });
            }
        }
    }

    fn on_tool_use(&mut self, id: String, name: String, input: Value) {
        let call = Call {
            summary: match name.as_str() {
                PLAN_TOOL => plan::title(input["plan"].as_str().unwrap_or_default()),
                ASK_TOOL => question::asked_summary(&input),
                _ => summarize(&input, &self.ws.dir().to_string_lossy()),
            },
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

    fn on_tool_result(&mut self, id: String, text: String, is_error: bool) {
        let call = (self.blocks.iter_mut().enumerate().rev()).find_map(|(bi, b)| match b {
            Block::Tool(c) => (c.id == id).then_some((bi, c)),
            Block::Tools { calls, .. } => calls.iter_mut().find(|c| c.id == id).map(|c| (bi, c)),
            _ => None,
        });
        if let Some((bi, c)) = call {
            if c.name == ASK_TOOL
                && let Some(answer) = question::answered(&text)
            {
                c.summary = answer.to_string();
            }
            c.result = Some(text.into());
            c.is_error = is_error;
            self.remeasure(bi); // an open call shows its output
        }
    }

    fn on_permission(
        &mut self,
        request_id: String,
        tool: String,
        tool_use_id: String,
        input: Value,
        cx: &mut Context<Self>,
    ) {
        // Questions wait for the user's answers in a card; the turn holds meanwhile.
        // (Replayed history has none: a request is only ever live.)
        if tool == ASK_TOOL && !self.replaying {
            return self.show_asked(request_id, tool_use_id, input, cx);
        }
        // Plan mode's approval happens in the plan pane, after the turn: the agent is
        // told the plan was shown, and the turn ends instead of waiting on a person.
        let reason = match tool.as_str() {
            PLAN_TOOL => {
                let text = input["plan"].as_str().unwrap_or_default().to_string();
                self.show_plan(tool_use_id, text, cx);
                PLAN_SHOWN
            }
            "EnterPlanMode" => ENTER_PLAN,
            _ => NEEDS_APPROVAL,
        };
        if let Some(a) = &mut self.agent
            && let Err(e) = a.deny(&request_id, reason)
        {
            self.blocks
                .push(Block::Info(format!("answering the agent failed: {e}")));
        }
    }

    fn on_result(
        &mut self,
        is_error: bool,
        text: String,
        cost: f64,
        secs: u64,
        cx: &mut Context<Self>,
    ) {
        self.busy = false;
        if !self.replaying {
            self.stopping = None; // a live interrupt's fallback outlives history
            self.drop_asked(cx);
        }
        let cut = std::mem::take(&mut self.cut);
        // A pruned or deleted session: claude says so in the result and exits, and
        // every resume would fail the same way. (Replay restores the session after.)
        let gone = is_error && text.starts_with(agent::SESSION_GONE);
        let took = duration(secs);
        let cost = turn_cost(&mut self.cost_total, cost);
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
            self.keep_reply();
            cx.emit(ChatEvent::TurnDone);
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
            return self.render_activity(cx).into_any_element();
        };
        if let Block::Text { src, .. } = &self.blocks[bi]
            && hidden_reply(src)
        {
            return div().id(ix).into_any_element();
        }
        let focused = self.focus.is_focused(window);
        let theme = cx.theme();
        let row = div()
            .id(ix)
            .group("row")
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
        let plan = self.plan.read(cx);
        let status = |c: &Call| match (&c.result, c.is_error) {
            (None, _) => Spinner::new().with_size(px(14.)).into_any_element(),
            // Denied on purpose: approval happens in the plan pane, answers in the chat.
            (Some(_), true) if !matches!(c.name.as_str(), PLAN_TOOL | ASK_TOOL) => {
                icon(IconName::X)
                    .text_color(theme.danger_foreground)
                    .into_any_element()
            }
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
                                .child(match c.name.as_str() {
                                    PLAN_TOOL => "Plan".to_string(),
                                    ASK_TOOL => "Question".to_string(),
                                    _ => c.name.clone(),
                                }),
                        )
                        .child(
                            div().flex_1().truncate().child(
                                match (c.name.as_str(), plan.number_of(&c.id)) {
                                    // The call's own input can be stale: claude fills
                                    // in the plan file when it asks, not when it calls.
                                    (PLAN_TOOL, Some(n)) => {
                                        format!(
                                            "v{n} · {}",
                                            plan::title(&plan.versions[n - 1].text)
                                        )
                                    }
                                    (ASK_TOOL, _) => (c.result.as_deref())
                                        .and_then(question::answered)
                                        .unwrap_or(&c.summary)
                                        .to_string(),
                                    _ => c.summary.clone(),
                                },
                            ),
                        )
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
                    // Clipped, with a "truncated" note, when parsed (crate::transcript::MAX_OUTPUT).
                    let result = c.result.clone();
                    let text = result.clone().unwrap_or("(running)".into());
                    d.child(
                        div()
                            .relative()
                            .mt_1()
                            .pl_3()
                            // The corner stays clear for the copy button.
                            .pr_8()
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
                            .child(text)
                            .when_some(result, |d, r| {
                                d.child(div().absolute().top_1().right_1().child(copy_button(
                                    ("copy-out", ix),
                                    r,
                                    "row",
                                )))
                            }),
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
                .child(
                    h_flex()
                        .items_start()
                        .gap_2()
                        .child(div().flex_1().min_w_0().child(t.clone()))
                        .child(copy_button(("copy", ix), t.clone(), "row").mr_neg_1()),
                )
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
            (Block::Review(r, queued), _) => column().child(self.review_card(
                r,
                ix,
                queued.then(|| "Queued · the agent reads it at its next step".into_any_element()),
                cx,
            )),
            (Block::Text { md, .. }, _) => column()
                .text_size(px(14.))
                .line_height(relative(1.5))
                .child(markdown(md, "row", link_roots(&self.ws))),
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
            // The line after a reply ("done · 48s") carries its copy button, like the action row
            // under a ChatGPT answer: always on the latest reply, on hover above it.
            (Block::Info(t), _) => {
                let reply = self.reply_above(bi).cloned();
                let latest = ix + 1 == self.rows.len();
                column()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t.clone())
                    .when_some(reply, |d, r| {
                        d.child(copy_button(("copy", ix), r, "row").when(latest, |d| d.visible()))
                    })
            }
        };
        row.child(inner).into_any_element()
    }

    /// The reply a turn's closing line ("done · 48s") sits under, if it ended on one.
    fn reply_above(&self, bi: usize) -> Option<&String> {
        match self.blocks.get(bi.checked_sub(1)?) {
            Some(Block::Text { src, .. }) => Some(src),
            _ => None,
        }
    }

    /// `y`: the selected row's text. A message, a reply (also from the line under it), a
    /// call's output or a submitted plan.
    pub(crate) fn copy_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(&(bi, call)) = self.rows.get(self.sel) else {
            return;
        };
        let text = match (&self.blocks[bi], call) {
            (Block::Info(t), _) => Some(self.reply_above(bi).unwrap_or(t).clone()),
            (b @ (Block::User(..) | Block::Review(..)), _) => b.message().map(|(t, _)| t.into()),
            (Block::Text { src, .. }, _) => Some(src.clone()),
            (Block::Tool(c), _) if c.name == PLAN_TOOL => {
                let plan = self.plan.read(cx);
                plan.number_of(&c.id)
                    .map(|n| plan.versions[n - 1].text.clone())
            }
            (Block::Tool(c), _) => c.result.as_ref().map(|r| r.to_string()),
            (Block::Tools { calls, .. }, Some(j)) => {
                calls[j].result.as_ref().map(|r| r.to_string())
            }
            (Block::Tools { .. }, None) => None,
        };
        if let Some(text) = text {
            crate::ui::copy_with(text, "Copied", window, cx);
        }
    }

    fn toggle(&mut self, cx: &mut Context<Self>) {
        let Some(&(bi, call)) = self.rows.get(self.sel) else {
            return;
        };
        // A submitted plan opens in the plan pane rather than showing the tool's reply.
        if let Block::Tool(c) = &self.blocks[bi]
            && c.name == PLAN_TOOL
        {
            let n = self.plan.read(cx).number_of(&c.id);
            if let Some(n) = n {
                self.plan.update(cx, |p, cx| p.show(n - 1, cx));
            }
            self.plan_open = true;
            cx.notify();
            return;
        }
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

/// A fresh Vim in Insert mode, so the composer types at once.
fn insert_mode() -> Vim {
    let mut v = Vim::default();
    v.mode = vim::Mode::Insert;
    v
}

impl Chat {
    /// The transcript before the first message: what to type, and the keys.
    fn empty_state(&self, cx: &App) -> Div {
        v_flex()
            .absolute()
            .inset_0()
            .items_center()
            .justify_center()
            .gap_2()
            .text_sm()
            .text_color(crate::ui::faint(cx.theme()))
            .child("Describe a task and press enter.")
            .child(
                h_flex()
                    .gap_3()
                    .child(hint("shift-tab", "plan mode"))
                    // Side questions fork a claude session; codex has none.
                    .when(!self.codex(), |d| {
                        d.child(hint("cmd-shift-enter", "side question"))
                    }),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(hint("c", "comment on a diff line"))
                    .child(hint("shift-c", "send comments")),
            )
    }

    /// The composer card, with the side question and the agent's questions above it.
    fn composer_bar(
        &self,
        asked_card: Option<AnyElement>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = cx.theme();
        let (radius, border, card, muted) = (
            theme.radius_lg,
            theme.border,
            theme.secondary,
            theme.muted_foreground,
        );
        // The editor's Vim pill, on `muted`: the card is already `secondary`.
        let vim_pill = self.vim_on.then(|| {
            div()
                .px_2()
                .h(px(20.))
                .flex()
                .items_center()
                .rounded_full()
                .font_family(theme.mono_font_family.clone())
                .bg(theme.muted)
                .child(self.vim.label())
        });
        div()
            .px_4()
            .pb_4()
            .pt_1()
            .children(self.render_side(window, cx))
            .children(asked_card)
            .child(
                v_flex()
                    .key_context("Composer")
                    // A key after a prefix that completes no binding (`ctrl-w j`) skips
                    // interceptors; catch it here, before the input types it.
                    .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                        let took = this.vim_took.take() == Some(ev.keystroke.clone());
                        if took
                            || (!window.has_pending_keystrokes()
                                && this.vim_intercept(&ev.keystroke, window, cx))
                        {
                            cx.stop_propagation();
                        }
                    }))
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
                        // The agent's questions first: the way back to the card.
                        if first_line
                            && let Some(f) = this.asked_form()
                            && c.focus_handle(cx).is_focused(window)
                        {
                            f.read(cx).focus_handle().clone().focus(window, cx);
                            cx.stop_propagation();
                            return;
                        }
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
                    // shift-tab: plan mode, as in Claude Code, instead of outdenting.
                    .capture_action(cx.listener(|this, _: &OutdentInline, _, cx| {
                        this.set_plan_mode(!this.meta.plan, cx);
                        cx.stop_propagation();
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
                                Button::new("plan-mode")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::ClipboardList)
                                    .label("Plan")
                                    .selected(self.meta.plan)
                                    .tooltip(if self.meta.plan {
                                        "Plan mode on: the agent plans and edits nothing (shift-tab)"
                                    } else {
                                        "Plan mode: plan first, build after you approve (shift-tab)"
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_plan_mode(!this.meta.plan, cx)
                                    })),
                            )
                            .when(self.codex(), |d| d.child(
                                Button::new("fast-mode")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Zap)
                                    .label("Fast")
                                    .selected(self.fast())
                                    .tooltip_with_action(
                                        "Fast: quicker replies that use more of your plan's usage",
                                        &ToggleFastMode,
                                        None,
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_fast(!this.fast(), cx)
                                    })),
                            ))
                            .child(
                                div().relative().min_w_0().child(
                                Button::new("model")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Sparkles)
                                    .label(self.model_chip())
                                    .dropdown_caret(true)
                                    // Shrinks to an ellipsis before the footer overflows.
                                    .flex_shrink(1.)
                                    .min_w_0()
                                    .max_w_full()
                                    .tooltip_with_action(
                                        match &self.reported {
                                            Some(r) => format!("Model and thinking · {r} · ⌘⇧. / ⌘⇧, raise or lower thinking"),
                                            None => "Model and thinking · ⌘⇧. / ⌘⇧, raise or lower thinking".into(),
                                        },
                                        &PickModel,
                                        None,
                                    )
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(PickModel.boxed_clone(), cx)
                                    }),
                                )
                                .child({
                                    let at = self.model_at.clone();
                                    canvas(move |b, _, _| at.set(b), |_, _, _, _| {})
                                        .absolute()
                                        .top_0()
                                        .left_0()
                                        .size_full()
                                }),
                            )
                            .children(
                                (self.model_menu.as_ref())
                                    .map(|(m, _)| crate::menu::layer(m.clone())),
                            )
                            .when_some(vim_pill, |d, p| d.child(p.flex_none()))
                            // Idle shows no keys: enter sends. Clipped, not wrapped, when narrow.
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .justify_end()
                                    .gap_3()
                                    .when(self.busy || self.hold, |d| {
                                        d.child(match self.editing {
                                            Some(_) => hint("enter", "save"),
                                            None => hint("enter", "queue"),
                                        })
                                    })
                                    .when(self.busy && !self.hold, |d| {
                                        d.child(hint("cmd-enter", "send now"))
                                    })
                                    .when(self.busy, |d| d.child(hint("ctrl-c", "interrupt"))),
                            ),
                    ),
            )
    }

    /// The dashed "Drop to attach" overlay for files dragged over the pane.
    fn drop_overlay(cx: &App) -> Div {
        let theme = cx.theme();
        let (radius, accent, muted) = (theme.radius_lg, theme.drag_border, theme.muted_foreground);
        let (bg, fg, drop_bg) = (
            theme.background.opacity(0.85),
            theme.foreground,
            theme.drop_target,
        );
        v_flex()
            .absolute()
            .inset_2()
            .items_center()
            .justify_center()
            .gap_1()
            .rounded(radius)
            .border_2()
            .border_dashed()
            .border_color(accent)
            .bg(bg)
            // The theme's drop tint over a faded transcript.
            .child(div().absolute().inset_0().rounded(radius).bg(drop_bg))
            .child(
                Icon::new(IconName::Paperclip)
                    .with_size(px(32.))
                    .text_color(accent)
                    .mb_2(),
            )
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(fg)
                    .child("Drop to attach"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("Files and images are added to your message"),
            )
    }
}

impl Render for Chat {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The queue emptied (a turn ended, a git step finished) while it had focus.
        if self.queue.is_empty() && self.qfocus.is_focused(window) {
            self.focus_composer(window, cx);
        }
        // Answered or gone (the turn ended) while it had the focus.
        self.build_asked(window, cx);
        if std::mem::take(&mut self.asked_focused) && self.asked.is_none() {
            self.focus_composer(window, cx);
        }
        let asked_card = self.render_asked(window, cx);
        self.asked_focused =
            asked_card.is_some() && (self.asked.as_ref()).is_some_and(|a| a.focused(window, cx));
        let border = cx.theme().border;
        let rows = list(
            self.list.clone(),
            cx.processor(|this, ix, window, cx| this.render_row(ix, window, cx)),
        )
        .size_full()
        .py_2();
        let main = v_flex()
            .id("chat-pane")
            .group("chat-pane")
            .relative()
            .size_full()
            .on_action(
                cx.listener(|this, _: &FocusComposer, window, cx| this.focus_composer(window, cx)),
            )
            // Files from Finder anywhere on the pane are copied in and mentioned in the composer.
            // Not `drag_over`/`on_drop`: gpui checks hover for those, and hover is off while the
            // last input was a key (a file drag doesn't count as the mouse), so a drag right
            // after typing showed nothing and dropped nothing.
            .on_drag_move(
                cx.listener(|this, e: &DragMoveEvent<ExternalPaths>, _, cx| {
                    let over =
                        (e.bounds.contains(&e.event.position)).then(|| e.drag(cx).paths().to_vec());
                    if over.is_some() != this.dropping.is_some() {
                        cx.notify();
                    }
                    this.dropping = over;
                }),
            )
            .on_file_drop_exit(cx.listener(|this, _, _, cx| {
                this.dropping = None;
                cx.notify();
            }))
            .child({
                let chat = cx.entity().downgrade();
                canvas(
                    |_, _, _| {},
                    move |_, _, window, _| {
                        let chat = chat.clone();
                        window.on_mouse_event(move |_: &MouseUpEvent, phase, window, cx| {
                            if phase != DispatchPhase::Bubble {
                                return;
                            }
                            chat.update(cx, |this, cx| {
                                if let Some(paths) = this.dropping.take() {
                                    this.attach(paths, None, window, cx);
                                    cx.notify();
                                }
                            })
                            .ok();
                        });
                    },
                )
                .absolute()
                .size_0()
            })
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
                    .on_action(
                        cx.listener(|this, _: &CopySelected, w, cx| this.copy_selected(w, cx)),
                    )
                    .on_action(cx.listener(|this, _: &FocusComposer, window, cx| {
                        this.focus_composer(window, cx)
                    }))
                    .child(rows)
                    .when(self.blocks.is_empty(), |d| d.child(self.empty_state(cx)))
                    .vertical_scrollbar(&self.list),
            )
            .child(self.composer_bar(asked_card, window, cx))
            // No hitbox, so it never takes the drop or the mouse. A drag that ends elsewhere
            // leaves no active drag, so a missed exit can't leave it up.
            .when(self.dropping.is_some() && cx.has_active_drag(), |d| {
                d.child(Self::drop_overlay(cx))
            });
        if !self.plan_open {
            return main.into_any_element();
        }
        h_flex()
            .size_full()
            // `i` in the plan pane: the pane is beside `main`, not inside it.
            .on_action(
                cx.listener(|this, _: &FocusComposer, window, cx| this.focus_composer(window, cx)),
            )
            .child(div().flex_1().min_w_0().h_full().child(main))
            .child(
                div()
                    .w(relative(0.48))
                    .min_w(px(340.))
                    .h_full()
                    .border_l_1()
                    .border_color(border)
                    .child(self.plan.clone()),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin

    #[test]
    fn turn_cost_is_the_delta_of_the_running_total() {
        let mut total = 0.;
        assert_eq!(turn_cost(&mut total, 0.5), 0.5);
        assert_eq!(turn_cost(&mut total, 0.75), 0.25);
        // A fresh process reports a lower total: shown as it is.
        assert_eq!(turn_cost(&mut total, 0.25), 0.25);
        // codex and failures report none: the total stays.
        assert_eq!(turn_cost(&mut total, 0.), 0.);
        assert_eq!(total, 0.25);
    }
}

#[cfg(all(test, feature = "test-support"))]
pub(crate) mod ui_tests {
    use super::*;
    use crate::models::effort_of;
    use critters::HOP_INTERVAL;
    use planmode::{APPROVED, BUILD_FRESH};

    /// Feeds recorded protocol lines to the chat as its process would.
    pub(crate) fn play(c: &mut Chat, lines: &[&str], cx: &mut Context<Chat>) {
        // Injected events replace the reader, including the fake process's exit during cleanup.
        c.reader = None;
        for l in lines {
            for ev in agent::parse(l) {
                c.apply(ev, cx);
            }
        }
    }
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin
    use gpui_kit::InputEvent as _;
    use gpui_kit::test::TestWindowExt as _;

    /// Real-renderer smoke check, also exporting the parade for visual inspection.
    pub(crate) fn render_activity_pixels(cx: &mut HeadlessAppContext) {
        use gpui_kit::test::TestEventExt as _;
        crate::state::TEST_HOME.get_or_init(|| {
            std::env::temp_dir().join(format!("shark-render-{}", std::process::id()))
        });
        struct ActivityPixels(Entity<Chat>);
        impl Render for ActivityPixels {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div()
                    .size_full()
                    .bg(cx.theme().background)
                    .flex()
                    .items_end()
                    .child(self.0.read(cx).render_activity(cx))
            }
        }
        let (window, view) = cx
            .update(|cx| {
                gpui_kit::open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            point(px(0.), px(0.)),
                            size(px(460.), px(200.)),
                        ))),
                        focus: false,
                        show: false,
                        ..Default::default()
                    },
                    cx,
                    |w, cx| {
                        cx.new(|cx| {
                            let chat = cx.new(|cx| {
                                Chat::new(
                                    Workspace::default(),
                                    ChatMeta {
                                        id: "activity-pixels".into(),
                                        ..Default::default()
                                    },
                                    false,
                                    w,
                                    cx,
                                )
                            });
                            cx.observe(&chat, |_, _, cx| cx.notify()).detach();
                            ActivityPixels(chat)
                        })
                    },
                )
            })
            .unwrap();
        let chat = cx.update(|cx| view.read(cx).0.clone());
        for (mode, label) in [
            (crate::state::Mode::Dark, "dark"),
            (crate::state::Mode::Light, "light"),
        ] {
            cx.update(|cx| {
                crate::theme::apply(
                    &crate::state::Appearance {
                        mode,
                        ..Default::default()
                    },
                    cx,
                );
                cx.set_reduce_motion(false);
                chat.update(cx, |c, cx| {
                    c.set_background(vec![], cx);
                    c.set_background(
                        (0..4)
                            .map(|i| agent::Task {
                                id: format!("agent-{i}"),
                                kind: "local_agent".into(),
                                description: format!("Background agent {}", i + 1),
                            })
                            .collect(),
                        cx,
                    );
                });
            });
            cx.run_until_parked();
            let mut first = None;
            for state in ["first", "next", "hover", "static"] {
                if state == "next" {
                    cx.advance_clock(HOP_INTERVAL);
                }
                if state == "static" {
                    cx.update(|cx| cx.set_reduce_motion(true));
                }
                cx.update_window(window, |_, w, cx| {
                    w.render_frame(cx);
                    if state == "hover" {
                        w.hover(("background-critter", 0usize), cx);
                    }
                    if state == "static" {
                        w.pointer_move(point(px(450.), px(0.)), None, cx);
                    }
                })
                .unwrap();
                if state == "hover" {
                    cx.advance_clock(std::time::Duration::from_secs(1));
                }
                let shot = cx
                    .capture_screenshot(window)
                    .expect("real activity renderer");
                if state == "next" {
                    assert_ne!(
                        first.as_ref().unwrap(),
                        &shot,
                        "the hop changes rendered pixels"
                    );
                }
                if state == "first" {
                    first = Some(shot.clone());
                }
                if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
                    let dir = PathBuf::from(dir);
                    std::fs::create_dir_all(&dir).unwrap();
                    shot.save(dir.join(format!("activity-{label}-{state}.png")))
                        .unwrap();
                }
            }
            cx.update(|cx| chat.update(cx, |c, cx| c.set_background(vec![], cx)));
        }
        cx.update(|cx| cx.set_reduce_motion(false));
        println!("activity pixels: dark/light hops, hover cards and reduced motion passed");
    }

    #[gpui_kit::test]
    fn background_hops_stop_and_history_preserves_live_tasks(cx: &mut TestAppContext) {
        let chat = open_chat("background-hop", cx);
        let tasks = || {
            (0..4)
                .map(|i| agent::Task {
                    id: format!("live-{i}"),
                    kind: "local_agent".into(),
                    description: format!("Background agent {i}"),
                })
                .collect()
        };
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                c.apply(Ev::Background(tasks()), cx);
                assert!(!c.busy && c.working());
                assert!(c.hopping.is_some());
                assert_eq!(c.list.item_count(), c.rows.len() + 1);
            })
        });
        let window = cx.windows()[0];
        let bounds = |cx: &mut TestAppContext| {
            cx.update_window(window, |_, w, cx| {
                w.render_frame(cx);
                assert_eq!(w.find("activity").bounds().size.height, px(24.));
                (0usize..4)
                    .map(|i| {
                        let slot = w.find(("background-critter", i)).bounds();
                        assert_eq!(slot.size, size(px(12.), px(12.)));
                        slot
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap()
        };
        let before = bounds(cx);
        cx.run_until_parked();
        cx.executor().advance_clock(HOP_INTERVAL);
        cx.run_until_parked();
        assert_eq!(
            bounds(cx),
            before,
            "hopping never moves the slots or transcript"
        );
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                assert_eq!(c.hop, 1);
                let critters = c.critters.clone();
                c.load_history(vec![Ev::Background(vec![]), Ev::Exited(String::new())], cx);
                assert_eq!(c.background, tasks());
                assert_eq!(c.critters, critters);
                assert_eq!(c.hop, 1);
                assert!(c.hopping.is_some());
                c.apply(Ev::Background(vec![]), cx);
                assert!(!c.working());
                assert!(c.hopping.is_none() && c.critters.is_empty());
                assert_eq!(c.list.item_count(), c.rows.len() + 1);
            })
        });
        cx.executor().advance_clock(HOP_INTERVAL * 3);
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(chat.read(cx).hop, 0, "no ticks after the tasks finish");
            cx.set_reduce_motion(true);
            chat.update(cx, |c, cx| {
                c.apply(Ev::Background(tasks()), cx);
                assert!(c.hopping.is_none(), "reduced motion never starts the timer");
            });
        });
        cx.executor().advance_clock(HOP_INTERVAL);
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(chat.read(cx).hop, 0);
            cx.set_reduce_motion(false);
            chat.update(cx, |c, cx| c.apply(Ev::Background(tasks()), cx));
        });
        cx.run_until_parked();
        cx.update(|cx| cx.set_reduce_motion(true));
        cx.executor().advance_clock(HOP_INTERVAL);
        cx.run_until_parked();
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                assert!(
                    c.hopping.is_none(),
                    "a changed motion preference stops the timer"
                );
                c.apply(Ev::Exited(String::new()), cx);
                assert!(!c.working() && c.critters.is_empty());
            })
        });
    }

    /// A claude stand-in that swallows stdin. Side questions and Ask Shark (both
    /// `--no-session-persistence`) get a canned answer, and their args are written next to it.
    const FAKE: &str = r#"#!/bin/sh
case "$*" in *--no-session-persistence*)
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
    pub(crate) fn fake_claude() -> std::path::PathBuf {
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
            model: Some("sonnet".into()),
            effort: Some("max".into()),
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
        assert!(args.contains("--model sonnet"), "the chat's model: {args}");
        assert!(args.contains("--effort max"), "and level: {args}");
        assert!(
            !crate::transcript::transcript_path(&meta.id).exists(),
            "no transcript"
        );
        let _ = std::fs::remove_file(ws.dir());
    }

    /// Ask Shark: a one-shot claude with the docs as its system prompt, no tools, saved nowhere.
    #[test]
    fn ask_process() {
        let fake = fake_claude();
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (mut a, rx) = agent::spawn_ask("haiku", "the docs").unwrap();
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
        assert!(args.contains("--model haiku"), "{args}");
        assert!(
            args.contains("--tools  --strict-mcp-config --no-session-persistence"),
            "{args}"
        );
        assert!(args.contains("--safe-mode"), "{args}");
        assert!(args.contains("--append-system-prompt the docs"), "{args}");
        assert!(!args.contains("--resume"), "{args}");
    }

    /// claude missing: the first message goes back to the composer and the error is readable.
    #[gpui_kit::test]
    fn first_message_survives_a_missing_claude(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let fake = fake_claude();
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        struct Restore(std::path::PathBuf);
        impl Drop for Restore {
            fn drop(&mut self) {
                // SAFETY: tests that spawn claude hold UI_LOCK, as this one does.
                unsafe { std::env::set_var("SHARK_CLAUDE", &self.0) };
            }
        }
        let _restore = Restore(fake);
        // SAFETY: as above.
        unsafe { std::env::set_var("SHARK_CLAUDE", "/nonexistent") };
        let root = std::env::temp_dir().join(format!("shark-noclaude-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let ws = Workspace {
            name: format!("noclaude-test-{}", std::process::id()),
            ..Default::default()
        };
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
                    cx.new(|cx| Chat::new(ws.clone(), meta, false, w, cx))
                })
            })
            .unwrap();
        cx.update_window(window, |_, w, cx| {
            let sent = chat.update(cx, |c, cx| c.ask("build the thing", w, cx));
            assert!(!sent);
        })
        .unwrap();
        cx.update(|cx| {
            let c = chat.read(cx);
            assert_eq!(c.composer.read(cx).value().as_ref(), "build the thing");
            let info = c.blocks.iter().find_map(|b| match b {
                Block::Info(t) => Some(t.as_str()),
                _ => None,
            });
            assert_eq!(
                info,
                Some("Couldn't start the agent: claude not found on PATH. See Home for setup.")
            );
        });
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                assert!(c.enqueue("commit", vec![], cx));
                assert_eq!(c.queue[0].text(), "commit", "failed sends stay queued");
                assert_eq!(c.composer.read(cx).value().as_ref(), "build the thing");
            })
        });
        let _ = std::fs::remove_file(ws.dir());
    }

    /// claude's `total_cost_usd` runs across turns (and resumes): each turn shows its share, in
    /// replayed history too, and a lower total counts as a reset.
    #[gpui_kit::test]
    fn turn_cost_is_the_running_totals_delta(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let ws = Workspace {
            name: format!("cost-test-{}", std::process::id()),
            ..Default::default()
        };
        let meta = ChatMeta {
            id: ws.name.clone(),
            ..Default::default()
        };
        let (_, chat) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
                    cx.new(|cx| Chat::new(ws.clone(), meta, false, w, cx))
                })
            })
            .unwrap();
        let result = |cost| Ev::Result {
            is_error: false,
            text: String::new(),
            cost,
            secs: 1,
        };
        let infos = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                (chat.read(cx).blocks.iter())
                    .filter_map(|b| match b {
                        Block::Info(t) => t.rsplit_once('$').map(|(_, c)| c.to_string()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
        };
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                c.load_history(vec![result(1.5), result(1.75)], cx);
                c.apply(result(2.0), cx); // a live turn goes on from the history's total
                c.apply(result(0.5), cx); // a new process starts over
                c.apply(result(0.), cx); // codex: no price
                c.apply(result(0.75), cx);
            })
        });
        assert_eq!(infos(cx), ["1.50", "0.25", "0.25", "0.50", "0.00", "0.25"]);
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
                    cx.new(|cx| Chat::new(ws.clone(), meta, false, w, cx))
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
        // Enter before the answer is in would drop the card and the question with it.
        act(cx, &|w, cx| w.press("enter", cx));
        cx.update(|cx| {
            let side = chat.read(cx).side.as_ref().expect("still asking");
            assert!(!side.done && side.question == "what is x?");
        });
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

        // A question that can't start (codex has no side questions) keeps the draft.
        act(cx, &|w, cx| {
            chat.update(cx, |c, cx| {
                c.meta.model = Some("codex:gpt-6-astra".into());
                c.composer.update(cx, |i, cx| i.set_value("keep me", w, cx));
                c.ask_draft_aside(w, cx);
            })
        });
        cx.update(|cx| {
            let c = chat.read(cx);
            assert_eq!(c.composer.read(cx).value().as_ref(), "keep me");
            let err = c.side.as_ref().and_then(|s| s.error.as_deref());
            assert!(err.is_some_and(|e| e.contains("Claude model")), "{err:?}");
        });
        let _ = std::fs::remove_file(ws.dir());
    }

    /// shift-tab turns plan mode on; each ExitPlanMode is a version with its changes marked;
    /// approving ends plan mode and tells the agent to build.
    #[gpui_kit::test]
    fn plan_mode(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::bind_keys(cx);
        });
        let root = std::env::temp_dir().join(format!("shark-plan-ui-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        fake_claude();
        let ws = Workspace {
            name: format!("plan-test-{}", std::process::id()),
            ..Default::default()
        };
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
                    cx.new(|cx| Chat::new(ws.clone(), meta, false, w, cx))
                })
            })
            .unwrap();
        cx.run_until_parked();
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
        act(cx, &|w, cx| w.press("shift-tab", cx));
        cx.update(|cx| {
            let c = chat.read(cx);
            assert!(c.meta.plan && c.plan_open, "shift-tab turns plan mode on");
        });
        act(cx, &|w, cx| w.input("plan a cache", cx));
        act(cx, &|w, cx| w.press("enter", cx));
        let submit = |cx: &mut TestAppContext, id: &str, plan: &str| {
            cx.update(|cx| {
                chat.update(cx, |c, cx| {
                    c.reader = None; // This fixture injects events instead of reading the fake process.
                    let input = serde_json::json!({"plan": plan});
                    c.apply(
                        Ev::ToolUse {
                            id: id.into(),
                            name: PLAN_TOOL.into(),
                            input: input.clone(),
                        },
                        cx,
                    );
                    c.apply(
                        Ev::Permission {
                            request_id: "r".into(),
                            tool: PLAN_TOOL.into(),
                            tool_use_id: id.into(),
                            input,
                        },
                        cx,
                    );
                    c.apply(
                        Ev::Result {
                            is_error: false,
                            text: String::new(),
                            cost: 0.,
                            secs: 0,
                        },
                        cx,
                    );
                })
            })
        };
        submit(cx, "t1", "# Cache\n\nUse a map.\n");
        submit(cx, "t2", "# Cache\n\nUse an LRU map.\n");
        act(cx, &|_, _| {});
        cx.update(|cx| {
            let p = chat.read(cx).plan.read(cx);
            assert_eq!(p.versions.len(), 2);
            assert_eq!((p.shown, p.changes), (1, true), "the new version, redlined");
            assert_eq!(p.number_of("t1"), Some(1));
        });
        // An older version on screen (h) isn't approved by `a`: only the latest is.
        let users = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                (chat.read(cx).blocks.iter())
                    .filter(|b| matches!(b, Block::User(t, _) if t == APPROVED))
                    .count()
            })
        };
        cx.update(|cx| chat.update(cx, |c, cx| c.plan.update(cx, |p, _| p.shown = 0)));
        act(cx, &|w, cx| chat.update(cx, |c, cx| c.approve_plan(w, cx)));
        cx.update(|cx| {
            let c = chat.read(cx);
            assert!(matches!(c.blocks.last(), Some(Block::Info(t)) if t.contains("older version")));
            assert_eq!(c.plan.read(cx).approved, None);
            assert!(c.meta.plan, "still planning");
        });
        cx.update(|cx| chat.update(cx, |c, cx| c.plan.update(cx, |p, _| p.shown = 1)));
        assert_eq!(users(cx), 0);
        act(cx, &|w, cx| chat.update(cx, |c, cx| c.approve_plan(w, cx)));
        cx.update(|cx| {
            let c = chat.read(cx);
            assert!(!c.meta.plan, "approving ends plan mode");
            assert!(!c.plan_open, "and closes the plan pane");
            assert_eq!(c.plan.read(cx).approved, Some(1));
            assert!(matches!(c.blocks.last(), Some(Block::User(t, _)) if t == APPROVED));
        });
        // Approved already: a second `a` or `n` says so instead of sending it again.
        cx.update(|cx| chat.update(cx, |c, _| c.busy = false));
        act(cx, &|w, cx| chat.update(cx, |c, cx| c.approve_plan(w, cx)));
        cx.update(|cx| {
            let c = chat.read(cx);
            assert!(
                matches!(c.blocks.last(), Some(Block::Info(t)) if t.contains("already approved"))
            );
            assert!(!c.busy);
        });
        assert_eq!(users(cx), 1, "approved once");
        act(cx, &|w, cx| {
            chat.update(cx, |c, cx| {
                c.busy = false;
                assert!(c.build_plan("# Cache\n\nUse an LRU map.\n", w, cx));
                assert_eq!(
                    c.meta.title, "Cache",
                    "titled after the plan, not the preamble"
                );
                let want = format!("{BUILD_FRESH}\n\n# Cache\n\nUse an LRU map.\n");
                assert!(matches!(c.blocks.last(), Some(Block::User(t, _)) if *t == want));
            })
        });
        cx.update(|cx| chat.update(cx, |c, _| c.reader = None));
        cx.run_until_parked(); // Drop the receiver before the OS thread sends the fake process's exit.
        cx.update(|cx| chat.update(cx, |c, _| c.agent = None));
        let _ = std::fs::remove_file(super::plan::store_path(&ws.name));
        let _ = std::fs::remove_file(ws.dir());
    }

    /// The agent's questions: a card the keys answer, its reply to the agent, and esc or a
    /// message from the composer to answer in the chat instead.
    #[gpui_kit::test]
    fn answers_questions(cx: &mut TestAppContext) {
        crate::state::TEST_HOME
            .get_or_init(|| std::env::temp_dir().join(format!("shark-ui-{}", std::process::id())));
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::bind_keys(cx);
        });
        let ws = Workspace {
            name: format!("ask-test-{}", std::process::id()),
            ..Default::default()
        };
        let meta = ChatMeta {
            id: ws.name.clone(),
            title: "ask".into(),
            // codex: its answers get a row the test can read (claude's come with its result).
            model: Some("codex:gpt-6.1-sol".into()),
            ..Default::default()
        };
        let (window, chat) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
                    cx.new(|cx| Chat::new(ws, meta, false, w, cx))
                })
            })
            .unwrap();
        let act = |cx: &mut TestAppContext, f: &dyn Fn(&mut Window, &mut App)| {
            cx.update_window(window, |_, w, cx| {
                f(w, cx);
                w.render_frame(cx);
            })
            .unwrap();
            // A picked answer shows for a moment before the form goes on.
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(500));
            cx.run_until_parked();
            cx.update_window(window, |_, w, cx| w.render_frame(cx))
                .unwrap();
        };
        let ask = |cx: &mut TestAppContext, id: &str| {
            cx.update(|cx| {
                chat.update(cx, |c, cx| {
                    c.busy = true;
                    c.agent_model = c.meta.model.clone(); // a codex process asks
                    c.apply(
                        Ev::Permission {
                            request_id: id.into(),
                            tool: ASK_TOOL.into(),
                            tool_use_id: format!("call-{id}"),
                            input: serde_json::json!({"questions": [
                                {"id": "db", "header": "DB", "question": "Which DB?", "isOther": true, "options": [{"label": "Postgres", "description": "SQL"}, {"label": "Redis", "description": "KV"}]},
                                {"id": "tests", "header": "Tests", "question": "Which tests?", "isOther": true, "options": [{"label": "Unit", "description": ""}, {"label": "E2E", "description": ""}]},
                            ]}),
                        },
                        cx,
                    );
                })
            })
        };
        let last_row = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                let c = chat.read(cx);
                match c.blocks.last() {
                    Some(Block::Tool(t)) if t.name == ASK_TOOL => Some(t.summary.clone()),
                    _ => None,
                }
            })
        };
        act(cx, &|w, cx| {
            chat.update(cx, |c, cx| c.focus_composer(w, cx))
        });
        ask(cx, "1");
        act(cx, &|_, _| {});
        cx.update_window(window, |_, w, cx| {
            let c = chat.read(cx);
            assert!(c.asking(), "the turn waits on the answers");
            assert!(
                c.asked.as_ref().unwrap().focused(w, cx),
                "an empty composer hands the card the focus"
            );
        })
        .unwrap();
        act(cx, &|w, cx| w.press("2", cx)); // Redis, then the next question
        act(cx, &|w, cx| w.press("1", cx)); // Unit, the last: submits
        assert_eq!(
            last_row(cx).as_deref(),
            Some(r#""Which DB?"="Redis", "Which tests?"="Unit""#)
        );
        cx.update_window(window, |_, w, cx| {
            let c = chat.read(cx);
            assert!(!c.asking());
            assert!(
                c.composer.read(cx).focus_handle(cx).is_focused(w),
                "back to the composer"
            );
        })
        .unwrap();

        // Your own text, enter in its field goes on; ↑ from the composer is the way back.
        ask(cx, "5");
        act(cx, &|_, _| {});
        act(cx, &|w, cx| {
            chat.update(cx, |c, cx| c.focus_composer(w, cx))
        });
        act(cx, &|w, cx| w.press("up", cx));
        cx.update_window(window, |_, w, cx| {
            assert!(
                chat.read(cx).asked.as_ref().unwrap().focused(w, cx),
                "↑ reaches the card"
            );
        })
        .unwrap();
        act(cx, &|w, cx| {
            let f = chat.read(cx).asked_form().unwrap();
            f.update(cx, |f, cx| f.focus_input("0", w, cx));
        });
        act(cx, &|w, cx| w.input("Mongo", cx));
        act(cx, &|w, cx| w.press("enter", cx));
        act(cx, &|w, cx| w.press("1", cx));
        assert_eq!(
            last_row(cx).as_deref(),
            Some(r#""Which DB?"="Mongo", "Which tests?"="Unit""#)
        );

        // esc: the agent is told the user answers in the chat.
        ask(cx, "2");
        act(cx, &|_, _| {});
        act(cx, &|w, cx| w.press("escape", cx));
        assert_eq!(last_row(cx).as_deref(), Some("Answered in the chat"));
        cx.update_window(window, |_, w, cx| {
            let c = chat.read(cx);
            assert!(!c.asking());
            assert!(c.composer.read(cx).focus_handle(cx).is_focused(w));
        })
        .unwrap();

        // A draft keeps the focus; sending it answers in the chat, and it waits for the turn.
        act(cx, &|w, cx| w.input("use sqlite", cx));
        ask(cx, "3");
        act(cx, &|_, _| {});
        cx.update_window(window, |_, w, cx| {
            let c = chat.read(cx);
            assert!(
                c.composer.read(cx).focus_handle(cx).is_focused(w),
                "typing isn't cut off"
            );
        })
        .unwrap();
        act(cx, &|w, cx| w.press("enter", cx));
        assert_eq!(last_row(cx).as_deref(), Some("Answered in the chat"));
        cx.update(|cx| {
            let c = chat.read(cx);
            assert!(!c.asking());
            assert_eq!(c.queued(), 1, "the message follows once the turn ends");
        });

        // The turn ending takes an unanswered card with it.
        ask(cx, "4");
        act(cx, &|_, _| {});
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                c.apply(
                    Ev::Result {
                        is_error: false,
                        text: String::new(),
                        cost: 0.,
                        secs: 0,
                    },
                    cx,
                );
                assert!(!c.asking());
                c.queue.clear();
            })
        });
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
                gpui_kit::open_window(options, cx, |w, cx| {
                    cx.new(|cx| Chat::new(ws, meta, false, w, cx))
                })
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
        let footer = |cx: &mut TestAppContext| {
            assert_eq!(
                list.item_count(),
                cx.update(|cx| chat.read(cx).rows.len()) + 1
            )
        };
        footer(cx);
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

        cx.update(|cx| chat.update(cx, |c, cx| c.sent("new".into(), None, cx)));
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
        footer(cx);
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
                    cx.new(|cx| Chat::new(ws, meta, false, w, cx))
                })
            })
            .unwrap();
        chat
    }

    /// Copy buttons: a message's shows on hover without moving, the latest reply's always
    /// shows and copies its markdown, and `y` copies the selected row.
    #[gpui_kit::test]
    fn copy_buttons(cx: &mut TestAppContext) {
        use gpui_kit::test::TestEventExt as _;
        let chat = open_chat("copy", cx);
        let window = cx.windows()[0];
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                c.apply(Ev::User("hi".into()), cx);
                c.apply(Ev::Text("**bold** reply".into()), cx);
                let result = Ev::Result {
                    is_error: false,
                    text: String::new(),
                    cost: 0.,
                    secs: 1,
                };
                c.apply(result, cx);
            })
        });
        let clip = |cx: &mut TestAppContext| cx.update(|cx| cx.read_from_clipboard()?.text());
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            let before = w.find(("copy", 0usize));
            assert!(!before.visible(), "hidden until hovered");
            // Over the message, left of the button: the row is what reveals it.
            let at = before.bounds().center() - point(px(100.), px(0.));
            w.pointer_move(at, None, cx);
            let after = w.find(("copy", 0usize));
            assert!(after.visible());
            assert_eq!(before.bounds(), after.bounds(), "showing it moves nothing");
            assert!(
                w.find(("copy", 2usize)).visible(),
                "the latest reply's always shows"
            );
            w.click(("copy", 2usize), cx);
        })
        .unwrap();
        assert_eq!(clip(cx).as_deref(), Some("**bold** reply"));
        cx.update_window(window, |_, w, cx| {
            chat.update(cx, |c, cx| {
                c.sel = 0;
                c.copy_selected(w, cx);
            })
        })
        .unwrap();
        assert_eq!(clip(cx).as_deref(), Some("hi"));
    }

    /// A codex model starts a new conversation (claude's session can't carry over) and keeps
    /// plan mode; another codex model keeps the thread.
    #[gpui_kit::test]
    fn codex_models_switch_backend(cx: &mut TestAppContext) {
        let chat = open_chat("codex", cx);
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                c.meta.session_id = Some("s1".into());
                c.set_plan_mode(true, cx);
                c.set_model("codex:gpt-6-astra", cx);
                assert_eq!(c.meta.session_id, None);
                assert!(c.meta.plan, "plan mode stays");
                c.meta.session_id = Some("th1".into());
                c.set_model("codex:gpt-6.1-sol", cx);
                assert_eq!(c.meta.session_id.as_deref(), Some("th1"));
                c.set_model("opus", cx);
                assert_eq!(c.meta.session_id, None);
                // The codex process still running reports its thread: not opus's to resume.
                c.agent_model = Some("codex:gpt-6.1-sol".into());
                let init =
                    r#"{"method":"turn/started","params":{"threadId":"th1","turn":{"id":"t1"}}}"#;
                for ev in agent::parse(init) {
                    c.apply(ev, cx);
                }
                assert_eq!(c.meta.session_id, None);
            })
        });
    }

    /// Thinking levels: `high` until picked, stepping stops at the ends, and a level the model
    /// lacks becomes its highest. A claude level change restarts the process at the next send;
    /// codex's doesn't.
    #[gpui_kit::test]
    fn thinking_levels(cx: &mut TestAppContext) {
        let chat = open_chat("effort", cx);
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                c.meta.model = Some("opus".into());
                assert_eq!((c.effort(), c.efforts().len()), ("high", 5));
                let odd = |e: &str| {
                    effort_of(&ChatMeta {
                        effort: Some(e.into()),
                        ..c.meta.clone()
                    })
                };
                assert_eq!(
                    (odd("minimal"), odd("ultra")),
                    ("high", "max"),
                    "never the costliest by accident"
                );
                assert_eq!(c.step_effort(-1, cx), "medium");
                assert_eq!(c.step_effort(-5, cx), "low");
                c.set_model("codex:gpt-6.1-sol", cx);
                assert_eq!(
                    c.meta.effort.as_deref(),
                    Some("low"),
                    "kept across backends"
                );
                c.set_effort("ultra", cx);
                assert_eq!(c.step_effort(1, cx), "ultra", "the top");
                c.set_model("fable", cx);
                assert_eq!(
                    c.meta.effort.as_deref(),
                    Some("max"),
                    "clamped to claude's top"
                );
                c.set_effort("ultra", cx);
                assert_eq!(c.effort(), "max");
                // What the send checks: claude's process is stale on another level.
                c.agent_model = c.meta.model.clone();
                c.agent_effort = "high";
                assert_ne!(c.agent_effort, c.effort());
            })
        });
    }

    /// A codex plan: its plan item is a version in the pane, which opens, and its row opens it
    /// again; history shows the row without adding the version twice. Fast follows codex's config
    /// until toggled.
    #[gpui_kit::test]
    fn codex_plan_mode_and_fast(cx: &mut TestAppContext) {
        let chat = open_chat("codex-plan", cx);
        let plan = r##"{"method":"item/completed","params":{"item":{"type":"plan","id":"t1-plan","text":"# Cache\n\nUse a map.\n"}}}"##;
        let started = r#"{"method":"item/started","params":{"item":{"type":"plan","id":"t1-plan","text":""}}}"#;
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                c.set_model("codex:gpt-6.1-sol", cx);
                c.agent_model = c.meta.model.clone();
                c.fast_by_config = true;
                assert!(c.fast() && c.meta.fast.is_none(), "codex's config, untouched");
                c.set_fast(false, cx);
                assert!(!c.fast() && c.meta.fast == Some(false));
                c.set_plan_mode(true, cx);
                assert!(c.meta.plan && c.plan_open);
                c.sent("plan a cache".into(), None, cx);
                // In codex's order: the reply after the plan streams before the plan completes.
                play(c, &[
                    r#"{"method":"turn/started","params":{"threadId":"th1","turn":{"id":"t1"}}}"#,
                    started,
                    r#"{"method":"item/agentMessage/delta","params":{"itemId":"m","delta":"Done"}}"#,
                    plan,
                    r#"{"method":"item/completed","params":{"item":{"type":"agentMessage","id":"m","text":"Done."}}}"#,
                    r#"{"method":"turn/completed","params":{"threadId":"th1","turn":{"id":"t1","status":"completed","durationMs":1000}}}"#,
                ], cx);
                let texts: Vec<(&str, bool)> = (c.blocks.iter())
                    .filter_map(|b| match b {
                        Block::Text { src, done, .. } => Some((src.as_str(), *done)),
                        _ => None,
                    })
                    .collect();
                assert_eq!(texts, [("Done.", true)], "the reply once, finished");
                let p = c.plan.read(cx);
                assert_eq!(p.versions.len(), 1);
                assert_eq!(p.number_of("t1-plan"), Some(1));
                assert!(c.plan_open && !c.busy);
                assert!(
                    c.blocks.iter().any(|b| matches!(b, Block::Tool(t) if t.name == PLAN_TOOL && t.summary == "Cache")),
                    "a Plan row"
                );
                c.plan_open = false;
                c.replaying = true;
                play(c, &[started, plan], cx);
                c.replaying = false;
                assert_eq!(c.plan.read(cx).versions.len(), 1, "history adds no version");
                assert!(!c.plan_open);
                plan::delete(&c.meta.id);
            })
        });
    }

    /// A replayed message that went out with workspace notes shows them as Info rows, not in
    /// the user's bubble.
    #[gpui_kit::test]
    fn replay_splits_workspace_notes_from_the_message(cx: &mut TestAppContext) {
        let chat = open_chat("note-replay", cx);
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                c.replaying = true;
                let wire = "[shark] New base: `api` now targets `dev` (was `main`).\n[shark] Repo `web` joined.\n\nfix it";
                c.apply(Ev::User(wire.into()), cx);
                c.replaying = false;
                let kinds: Vec<String> = (c.blocks.iter())
                    .map(|b| match b {
                        Block::Info(t) => format!("info {t}"),
                        Block::User(t, _) => format!("user {t}"),
                        _ => "other".into(),
                    })
                    .collect();
                assert_eq!(
                    kinds,
                    [
                        "info [shark] New base: `api` now targets `dev` (was `main`).",
                        "info [shark] Repo `web` joined.",
                        "user fix it"
                    ]
                );
            })
        });
    }

    /// A codex turn as the chat sees it: a message sent mid-turn joins the running turn with no
    /// new init, and once read nothing stays unread or steered. One codex refuses is never
    /// echoed, and doesn't hold up the next.
    #[gpui_kit::test]
    fn codex_turn_reads_mid_turn_messages(cx: &mut TestAppContext) {
        let chat = open_chat("codex-turn", cx);
        let echo = |t: &str| {
            format!(
                r#"{{"method":"item/started","params":{{"item":{{"type":"userMessage","id":"u","content":[{{"type":"text","text":"{t}"}}]}}}}}}"#
            )
        };
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                c.meta.model = Some("codex:gpt-6-astra".into());
                c.agent_model = c.meta.model.clone();
                c.sent("first".into(), None, cx);
                c.steered = true; // what a cmd-enter send does mid-turn
                c.sent("second".into(), None, cx);
                play(c, &[
                    r#"{"method":"turn/started","params":{"threadId":"th9","turn":{"id":"t1"}}}"#,
                    &echo("first"),
                    r#"{"method":"item/completed","params":{"item":{"type":"agentMessage","id":"m","text":"FIRST"}}}"#,
                    &echo("second"),
                    r#"{"method":"turn/completed","params":{"threadId":"th9","turn":{"id":"t1","items":[],"status":"completed","durationMs":2000}}}"#,
                ], cx);
                assert_eq!(c.meta.session_id.as_deref(), Some("th9"));
                assert!(!c.busy && c.unread.is_empty() && !c.steered);
                c.sent("refused".into(), None, cx);
                play(c, &[r#"{"id":3,"error":{"code":-32600,"message":"bad input"}}"#], cx);
                c.sent("next".into(), None, cx);
                play(c, &[&echo("next")], cx);
                let users: Vec<(&str, bool)> = (c.blocks.iter())
                    .filter_map(|b| match b {
                        Block::User(t, q) => Some((t.as_str(), *q)),
                        _ => None,
                    })
                    .collect();
                assert_eq!(
                    users,
                    [("first", false), ("second", false), ("refused", false), ("next", false)],
                    "read in place, none twice"
                );
            })
        });
    }

    /// Calls cut off by an exit stop spinning; a missing session is dropped, an early exit isn't.
    #[gpui_kit::test]
    fn exits_end_calls_and_drop_missing_sessions(cx: &mut TestAppContext) {
        let chat = open_chat("exit", cx);
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                // A turn ended by a crash or a new conversation has no reply of the last one.
                c.last_reply = "old".into();
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
                assert_eq!(c.last_reply, "");
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
                (c.busy, c.last_reply) = (true, "old".into());
                c.new_conversation(cx);
                assert_eq!(c.last_reply, "");
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
                c.sent("first".into(), None, cx);
                c.sent("steer".into(), None, cx);
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
                c.sent("steer".into(), None, cx);
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

    /// What the CLI writes for /compact is notices, not the user's messages, live or replayed;
    /// a slash command's echo folds into the bubble that was typed.
    #[gpui_kit::test]
    fn cli_lines_are_not_user_bubbles(cx: &mut TestAppContext) {
        let lines = [
            r#"{"type":"user","isSynthetic":true,"isReplay":false,"message":{"content":"This session is being continued from a previous conversation..."}}"#,
            r#"{"type":"user","isReplay":true,"message":{"content":"<local-command-stdout>Compacted </local-command-stdout>"}}"#,
            r#"{"type":"result","subtype":"success","local_command":"compact","is_error":false,"result":"","total_cost_usd":0,"duration_ms":0}"#,
        ];
        let evs = || lines.iter().flat_map(|l| crate::agent::parse(l));
        let infos = |c: &Chat| {
            (c.blocks.iter())
                .filter_map(|b| match b {
                    Block::Info(t) => Some(t.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let live = open_chat("compact-live", cx);
        cx.update(|cx| {
            live.update(cx, |c, cx| {
                c.busy = true; // a message sent mid-turn is queued until echoed
                c.sent("/compact".into(), None, cx);
                assert!(matches!(&c.blocks[0], Block::User(_, true)));
                evs().for_each(|e| c.apply(e, cx));
                assert!(matches!(&c.blocks[0], Block::User(_, true)), "not echoed yet");
                let echo = r#"{"type":"user","isReplay":true,"message":{"content":"<command-message>compact</command-message>\n<command-name>/compact</command-name>\n<command-args></command-args>"}}"#;
                crate::agent::parse(echo).into_iter().for_each(|e| c.apply(e, cx));
                let users: Vec<_> = (c.blocks.iter())
                    .filter(|b| matches!(b, Block::User(..)))
                    .collect();
                assert!(matches!(users[..], [Block::User(t, false)] if t == "/compact"));
                assert_eq!(infos(c)[..2], ["Conversation compacted", "Compacted"]);
            })
        });
        let replay = open_chat("compact-replay", cx);
        cx.update(|cx| {
            replay.update(cx, |c, cx| {
                c.load_history(evs().collect(), cx);
                assert!(!c.blocks.iter().any(|b| matches!(b, Block::User(..))));
                assert_eq!(infos(c)[..2], ["Conversation compacted", "Compacted"]);
            })
        });
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
        let (dir, transcript) = (ws.dir(), crate::transcript::transcript_path(&meta.id));
        let (window, chat) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
                    cx.new(|cx| Chat::new(ws, meta, false, w, cx))
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
        act(cx, &|w, cx| w.press("up", cx));
        act(cx, &|w, cx| w.press("alt-up", cx));
        assert_eq!(queue(cx), ["second", "first", "third"]);
        assert_eq!(qsel(cx), 0, "the cursor follows the moved row");
        act(cx, &|w, cx| w.press("alt-up", cx));
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

        // Codex reports an interrupted turn with a non-error result.
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                c.apply(Ev::Interrupted, cx);
                c.apply(result(false), cx);
            })
        });
        assert_eq!(queue(cx), ["first", "second edited"]);
        assert!(
            users(cx).is_empty(),
            "interruption must not send queued messages"
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
        act(cx, &|w, cx| w.press("up", cx));
        act(cx, &|w, cx| w.press("e", cx));
        assert_eq!(value(cx), "b");
        act(cx, &|w, cx| w.press("escape", cx));
        act(cx, &|w, cx| chat.update(cx, |c, cx| c.focus_queue(w, cx)));
        act(cx, &|w, cx| w.press("home", cx));
        act(cx, &|w, cx| w.press("enter", cx));
        assert_eq!(queue(cx), ["c"]);
        act(cx, &|w, cx| {
            chat.update(cx, |c, cx| c.focus_composer(w, cx))
        });
        act(cx, &|w, cx| w.press("enter", cx));
        assert_eq!(queue(cx), ["b", "c"], "b keeps its place ahead of c");

        // A review queued in front moves the rows down: the cursor and an edited slot follow.
        let review = |p: &str| ReviewCard {
            prompt: p.into(),
            items: vec![],
            changes: String::new(),
        };
        act(cx, &|w, cx| chat.update(cx, |c, cx| c.focus_queue(w, cx)));
        act(cx, &|w, cx| w.press("end", cx));
        cx.update(|cx| chat.update(cx, |c, cx| c.send_review(review("R1"), cx)));
        assert_eq!(queue(cx)[qsel(cx)], "c", "still on c");
        act(cx, &|w, cx| w.press("e", cx));
        assert_eq!(value(cx), "c");
        cx.update(|cx| chat.update(cx, |c, cx| c.send_review(review("R2"), cx)));
        act(cx, &|w, cx| w.press("enter", cx));
        assert_eq!(queue(cx), ["R2", "R1", "b", "c"], "c back in its slot");

        // Git prompts append, preserve drafts/edits, and wait behind a git hold.
        let acknowledged = Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let _ack = cx.update(|cx| {
            let acknowledged = acknowledged.clone();
            cx.subscribe(&chat, move |_, ev, _| {
                if let ChatEvent::GitReviewsSent(ids) = ev {
                    acknowledged.borrow_mut().extend(ids.iter().cloned());
                }
            })
        });
        act(cx, &|w, cx| {
            chat.update(cx, |c, cx| {
                c.queue.clear();
                c.composer.update(cx, |t, cx| t.set_value("draft", w, cx));
                assert!(c.enqueue("commit", vec!["g1".into()], cx));
                assert!(c.enqueue("fix checks", vec!["g2".into()], cx));
            });
        });
        assert_eq!(queue(cx), ["commit", "fix checks"]);
        assert!(
            acknowledged.borrow().is_empty(),
            "queuing does not acknowledge reviews"
        );
        assert_eq!(value(cx), "draft");
        act(cx, &|w, cx| {
            chat.update(cx, |c, cx| {
                c.composer.update(cx, |t, cx| t.set_value("", w, cx));
                c.qsel = 0;
                c.edit_queued(w, cx);
                c.busy = false;
                assert!(c.enqueue("update base", vec![], cx));
                assert_eq!(c.editing, Some(0));
            });
        });
        assert_eq!(value(cx), "commit");
        assert_eq!(queue(cx), ["fix checks", "update base"]);
        act(cx, &|w, cx| {
            chat.update(cx, |c, cx| {
                c.hold = true;
                c.send(false, w, cx);
            });
        });
        assert_eq!(queue(cx), ["commit", "fix checks", "update base"]);
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                assert!(c.enqueue("restack", vec![], cx));
                c.hold = false;
                c.drain(cx);
            })
        });
        assert_eq!(users(cx).last().map(String::as_str), Some("commit"));
        assert_eq!(*acknowledged.borrow(), ["g1"], "metadata survives an edit");
        assert_eq!(queue(cx), ["fix checks", "update base", "restack"]);
        cx.update(|cx| chat.update(cx, |c, cx| c.apply(result(false), cx)));
        assert_eq!(users(cx).last().map(String::as_str), Some("fix checks"));
        assert_eq!(*acknowledged.borrow(), ["g1", "g2"]);
        act(cx, &|w, cx| {
            chat.update(cx, |c, cx| {
                c.enqueue("cancel me", vec!["g3".into()], cx);
                c.delete_queued(w, cx);
            })
        });
        assert_eq!(
            *acknowledged.borrow(),
            ["g1", "g2"],
            "deleted reviews remain unhandled"
        );
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                c.archiving = true;
                assert!(!c.enqueue("must not queue", vec![], cx));
            })
        });
        assert_eq!(queue(cx), ["update base", "restack"]);

        cx.update(|cx| chat.update(cx, |c, _| c.reader = None));
        cx.run_until_parked(); // Drop the receiver before the OS thread sends the fake process's exit.
        cx.update(|cx| chat.update(cx, |c, _| c.agent = None));
        let _ = std::fs::remove_file(dir);
        let _ = std::fs::remove_file(transcript);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// `@` opens the file menu; keys drive it and a pick leaves a repo-qualified chip.
    #[gpui_kit::test]
    fn dropping_a_file_attaches_it(cx: &mut TestAppContext) {
        // Its workspace dir is under the UI tests' home, which they wipe.
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
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
                    cx.new(|cx| Chat::new(ws, meta, false, w, cx))
                })
            })
            .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            let size = w.viewport_size();
            let position = point(size.width / 2., size.height / 2.);
            let paths = ExternalPaths(vec![shot.clone()].into());
            // After a key the drag still shows the overlay: gpui's hover styles wouldn't.
            w.dispatch_keystroke(Keystroke::parse("a").unwrap(), cx);
            w.dispatch_event(
                FileDropEvent::Entered { position, paths }.to_platform_input(),
                cx,
            );
            w.dispatch_event(FileDropEvent::Pending { position }.to_platform_input(), cx);
        })
        .unwrap();
        cx.update(|cx| {
            assert!(
                chat.read(cx).dropping.is_some(),
                "overlay while files hover"
            )
        });
        cx.update_window(window, |_, w, cx| {
            let position = point(w.viewport_size().width / 2., w.viewport_size().height / 2.);
            w.dispatch_event(FileDropEvent::Submit { position }.to_platform_input(), cx);
        })
        .unwrap();
        cx.update(|cx| assert!(chat.read(cx).dropping.is_none(), "gone after the drop"));
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
            branch: None,
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
                    cx.new(|cx| Chat::new(ws, meta, false, w, cx))
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

        // `#` lists PRs; enter picks only what the query names outright.
        let prs: Vec<complete::Pr> = serde_json::from_str(
            r#"[{"repo":"api","number":12,"title":"Login page","author":{"login":"kim"},
                 "url":"https://github.com/o/api/pull/12","state":"OPEN","updatedAt":"1"},
                {"repo":"web","number":7,"title":"Fix endpoint","author":null,
                 "url":"https://github.com/o/web/pull/7","state":"MERGED","updatedAt":"2"}]"#,
        )
        .unwrap();
        cx.update(|cx| {
            chat.update(cx, |c, _| {
                c.pr_items = complete::pr_items(&prs);
                c.prs = prs.clone();
            })
        });
        let armed =
            |cx: &mut TestAppContext| cx.update(|cx| chat.read(cx).menu.as_ref().map(|m| m.armed));
        // Fuzzily "Fix endpoint", like `#endif` or `#[test]` might match in code.
        act(cx, &|w, cx| w.input(" #fixend", cx));
        assert_eq!(menu(cx), Some((1, 0)));
        assert_eq!(
            armed(cx),
            Some(false),
            "a fuzzy match isn't picked by enter"
        );
        act(cx, &|w, cx| w.press("escape", cx));
        act(cx, &|w, cx| w.input(" #12", cx));
        assert_eq!(armed(cx), Some(true));
        act(cx, &|w, cx| w.press("enter", cx));
        assert!(
            value(cx).ends_with(" https://github.com/o/api/pull/12 "),
            "{}",
            value(cx)
        );
        cx.update(|cx| {
            let c = chat.read(cx).composer.read(cx);
            assert_eq!(c.tokens().last().unwrap().token().label(), "api#12");
        });
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui_kit::test]
    fn vim_in_the_composer(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::bind_keys(cx);
        });
        let ws = Workspace {
            name: format!("vim-test-{}", std::process::id()),
            branch: "b".into(),
            ..Default::default()
        };
        let meta = ChatMeta {
            id: ws.name.clone(),
            ..Default::default()
        };
        let (window, chat) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
                    cx.new(|cx| Chat::new(ws, meta, true, w, cx))
                })
            })
            .unwrap();
        cx.run_until_parked();
        let act = |cx: &mut TestAppContext, f: &dyn Fn(&mut Window, &mut App)| {
            cx.update_window(window, |_, w, cx| {
                f(w, cx);
                w.render_frame(cx); // the input binds its actions as it renders
            })
            .unwrap()
        };
        let keys = |cx: &mut TestAppContext, keys: &[&str]| {
            act(cx, &|w, cx| keys.iter().for_each(|k| w.press(k, cx)))
        };
        let value = |cx: &mut TestAppContext| {
            cx.update(|cx| chat.read(cx).composer.read(cx).value().to_string())
        };
        let mode = |cx: &mut TestAppContext| cx.update(|cx| chat.read(cx).vim.mode);
        let in_transcript = |cx: &mut TestAppContext| {
            cx.update_window(window, |_, w, cx| chat.read(cx).focus.is_focused(w))
                .unwrap()
        };
        let queue = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                (chat.read(cx).queue.iter())
                    .map(Queued::text)
                    .collect::<Vec<_>>()
            })
        };
        cx.update(|cx| chat.update(cx, |c, _| c.busy = true)); // Enter queues, no process
        act(cx, &|w, cx| {
            chat.update(cx, |c, cx| c.focus_composer(w, cx))
        });

        act(cx, &|w, cx| w.input("hello world", cx));
        assert_eq!(value(cx), "hello world", "starts in Insert mode");
        keys(cx, &["escape"]);
        assert_eq!(mode(cx), vim::Mode::Normal);
        assert!(
            !in_transcript(cx),
            "the first escape only leaves Insert mode"
        );
        keys(cx, &["0", "x"]);
        assert_eq!(value(cx), "ello world");
        keys(cx, &["u"]);
        assert_eq!(value(cx), "hello world", "u undoes");
        keys(cx, &["0", "x", "j", "q"]);
        assert_eq!(value(cx), "ello world", "Normal mode edits, never types");
        keys(cx, &["shift-a"]);
        act(cx, &|w, cx| w.input("!", cx));
        assert_eq!(value(cx), "ello world!");
        keys(cx, &["enter"]);
        assert_eq!(queue(cx), ["ello world!"], "enter sends from Insert mode");
        assert_eq!(value(cx), "");

        act(cx, &|w, cx| w.input("two", cx));
        keys(cx, &["escape", "enter"]);
        assert_eq!(queue(cx), ["ello world!", "two"], "and from Normal mode");
        assert_eq!(
            mode(cx),
            vim::Mode::Insert,
            "sending goes back to Insert mode"
        );

        keys(cx, &["escape", "escape"]);
        assert!(
            in_transcript(cx),
            "escape in Normal mode leaves the composer"
        );

        // Off: escape leaves at once, and letters type.
        cx.update(|cx| chat.update(cx, |c, cx| c.set_vim(false, cx)));
        act(cx, &|w, cx| {
            chat.update(cx, |c, cx| c.focus_composer(w, cx))
        });
        act(cx, &|w, cx| w.input("jk", cx));
        keys(cx, &["escape"]);
        assert_eq!(value(cx), "jk");
        assert!(in_transcript(cx));
    }
}
