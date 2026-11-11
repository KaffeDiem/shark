//! One agent conversation (a chat tab): transcript blocks, composer, and the claude process.
mod completion;
#[cfg(all(test, feature = "test-support"))]
pub(crate) mod completion_pixels;
mod critters;
mod handoff;
pub(crate) use handoff::HandoffTarget;
mod model;
mod planmode;
mod question;
mod queue;
mod review;
mod side;
mod tools;

use self::queue::Queued;
use crate::complete::{self, Item};
use crate::harness::{self, Ev, Harness};
use crate::models::EFFORT;
use crate::models::{effort_label, model_label};
use crate::plan::{self, PlanPane};
use crate::review_protocol::{ReviewCard, card_of, hidden_reply, hide_reports, may_report};
use crate::state::{ChatMeta, REPO_CONTEXT, Task, context_record};
use crate::ui::{copy_button, link_roots, markdown, model_entries};
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
use gpui_kit::component::text::TextViewState;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme, Icon, Selectable as _, ThemeColor, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use planmode::{ENTER_PLAN, NEEDS_APPROVAL, PLAN_SHOWN};
use review::{NOTES_END, info_notes, review_title, split_notes};
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
    AttachCompletionIssue(crate::linear::Hit),
    SearchCompletionIssues(String),
    /// Chat metadata changed; the app saves it.
    Meta,
    /// The chat received its first title; only a live user send carries a prompt.
    Title(Option<String>),
    /// The user picked the model or thinking level (after `Meta`); new chats start on it.
    Picked,
    TurnDone,
    HandOff {
        to: Option<String>,
    },
    /// The agent button: the app lists agents to pick, or to hand off to once the chat started.
    AgentMenu,
    GitReviewsSent(Vec<String>),
    /// The agent started or stopped waiting on answers to its questions (`Chat::asking`).
    Asking,
    /// `refuse_unready` said no: the app offers Attach repository.
    NeedsRepo,
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
    pub ws: Task,
    pub meta: ChatMeta,
    pub(crate) next_agent: Option<HandoffTarget>,
    handoff_dismissed: bool,
    blocks: Vec<Block>,
    /// What the list shows, derived from `blocks`; see `sync_rows`.
    rows: Vec<Row>,
    sel: usize,
    pub busy: bool,
    /// Background tasks (subagents, shells) still running; the turn can end before they do.
    pub background: Vec<harness::BackgroundTask>,
    critters: HashMap<String, usize>,
    hop: usize,
    hopping: Option<gpui_kit::Task<()>>,
    /// When the running turn started.
    pub started: Option<Instant>,
    /// Set while a git step runs in these worktrees; sends wait for it.
    pub hold: bool,
    /// Set while the agent has an attachment operation (`Task::attaching`). Its own field:
    /// a git step ending must not release it, nor it a git step.
    pub attach_hold: bool,
    /// Set while the task's worktrees are being removed; sends are refused.
    pub archiving: bool,
    /// What `ChatMeta::delivered` becomes once the agent echoes the message at a block: a message
    /// it never read leaves the context pending.
    told: Vec<(usize, std::collections::BTreeMap<String, u64>, String)>,
    harness: Option<Harness>,
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
    /// The agent button's bounds, where the app anchors its menu.
    pub(crate) agent_at: std::rc::Rc<std::cell::Cell<Bounds<Pixels>>>,
    /// Model and (claude) thinking level the running process was started with; a mismatch
    /// respawns it on the next send.
    harness_model: Option<String>,
    harness_effort: &'static str,
    /// Repo directories passed to the running Claude (`git::claude_skill_args`).
    harness_skills: Vec<String>,
    /// The agent prompt the process started with (`git::agent_prompt`).
    harness_agent: Option<String>,
    /// Virtualized: only visible rows render, which matters while a spinner redraws every frame.
    list: ListState,
    pub focus: FocusHandle,
    reader: Option<gpui_kit::Task<()>>,
    files: Vec<Item>,
    commands: Vec<Item>,
    menu: Option<Menu>,
    menu_scroll: ScrollHandle,
    indexing: Option<gpui_kit::Task<()>>,
    probing: Option<gpui_kit::Task<()>>,
    /// PRs of every repo, as last listed; cached on disk per source repository.
    prs: Vec<complete::Pr>,
    pr_items: Vec<Item>,
    fetching_prs: Option<gpui_kit::Task<()>>,
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
    /// SIGTERM fallback for an interrupt claude doesn't act on.
    stopping: Option<gpui_kit::Task<()>>,
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
pub(crate) fn title_of(text: &str) -> String {
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
) -> gpui_kit::Task<()> {
    cx.spawn(async move |this, cx| {
        while let Ok(ev) = rx.recv().await {
            if this.update(cx, |t, cx| apply(t, ev, cx)).is_err() {
                break;
            }
        }
    })
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
        ws: Task,
        mut meta: ChatMeta,
        vim_on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Every chat passes here, new, reopened or restored: none starts out of Plan mode
        // without a repository. Not `ready`: a later attachment leaves a chat's mode alone.
        meta.plan |= ws.repos.is_empty();
        let composer = cx.new(|cx| {
            crate::task_composer::textarea(window, cx, 1)
                .placeholder("Message the agent (@ to add files, / for skills)")
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
        let attach_hold = !ws.attaching.is_empty();
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
            next_agent: None,
            handoff_dismissed: false,
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
            attach_hold,
            archiving: false,
            told: vec![],
            harness: None,
            composer,
            last_reply: String::new(),
            last_turn: 0,
            vim: insert_mode(),
            vim_on,
            vim_took: None,
            model_menu: None,
            model_at: Default::default(),
            agent_at: Default::default(),
            harness_model: None,
            harness_effort: EFFORT,
            harness_skills: Vec::new(),
            harness_agent: None,
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
        let told = std::mem::take(&mut self.told);
        // state.json's session (it may have been reset), or the live process's
        let kept = (
            self.busy,
            std::mem::take(&mut self.background),
            std::mem::take(&mut self.critters),
            self.hop,
            self.hopping.take(),
            self.started,
            self.cut,
            self.steered,
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
            self.critters,
            self.hop,
            self.hopping,
            self.started,
            self.cut,
            self.steered,
            self.meta.session_id,
        ) = kept;
        let n = self.blocks.len();
        self.blocks.extend(live);
        self.unread = unread.into_iter().map(|i| i + n).collect();
        self.told = told
            .into_iter()
            .map(|(bi, record, wire)| (bi + n, record, wire))
            .collect();
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
            cx.emit(ChatEvent::Title(None));
        }
        cx.notify();
    }

    /// Whether anything is still going on: a turn, or tasks it left running in the background.
    pub fn working(&self) -> bool {
        self.busy || !self.background.is_empty()
    }

    /// Nothing is running or about to: no turn, background task or question, and no message sent
    /// into a turn that ended before reading it, which starts one of its own.
    pub fn idle(&self) -> bool {
        !self.working() && !self.asking() && !self.steered
    }

    /// Why sends wait, if they do, and what to do about it. Every path that sends asks this,
    /// through `send_block` at least.
    pub fn held(&self) -> Option<String> {
        use crate::state::AttachStage::*;
        if self.hold {
            return Some("a git step is running".into());
        }
        if !self.attach_hold {
            return None;
        }
        let list = &self.ws.attaching;
        let Some(a) = list.iter().find(|a| a.stuck()).or(list.first()) else {
            return Some("a repository is being attached".into());
        };
        let repo = &a.repo;
        let chip = "its chip in the header (or Attach repository… in cmd-k)";
        Some(match a.stage {
            SetupFailed => format!("setup of {repo} failed; open {chip} to retry or continue"),
            Uncertain => format!("Shark can't tell how {repo} ended up; open {chip} to check"),
            Queued => format!("{repo} is queued to attach; {chip} says what it waits for"),
            Creating | Setup => format!("{repo} is being attached; {chip} shows how far it got"),
        })
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
        if crate::task_composer::paste_reference(&self.composer, item, window, cx) {
            return true;
        }
        if let Some(text) = item
            .text()
            .filter(|text| complete::pasted_reference(text).is_some())
        {
            cx.emit(ChatEvent::SearchCompletionIssues(text));
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
        let held = self.held().is_some();
        let sent = !held && (!busy || now) && self.send_text(text, now, cx);
        if !sent && (self.busy || held) {
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

    /// Feeds `ev` to the chat as its process would.
    #[cfg(all(test, feature = "test-support"))]
    pub(crate) fn play(&mut self, ev: Ev, cx: &mut Context<Self>) {
        // Injected events replace the reader: a real exit at cleanup trips the test scheduler.
        self.reader = None;
        self.apply(ev, cx);
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
        let had_draft = !draft.is_empty();
        // The same prompt put back below after a failed start: retrying must not send it twice.
        if draft == prompt.trim() {
            draft.clear();
        }
        let text = match draft.is_empty() {
            true => prompt.to_string(),
            false => format!("{prompt}\n\nAlso: {draft}"),
        };
        let sent = self.send_text(text.clone(), false, cx);
        if sent && had_draft {
            cx.emit(ChatEvent::GitReviewsSent(std::mem::take(
                &mut self.editing_git_reviews,
            )));
            self.finish_draft(window, cx);
        }
        // It didn't go out (refusals keep a hold or `archiving` set): a new chat's first message,
        // e.g. from Home, goes back to the composer instead of being lost.
        let failed = !sent && !self.busy && self.held().is_none() && !self.archiving;
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
        if let Some(why) = self.held() {
            let msg = format!("{why}; send again when it finishes");
            self.push(Block::Info(msg), cx);
            return false;
        }
        if self.archiving {
            self.push(
                Block::Info("this task is being deleted; nothing was sent".into()),
                cx,
            );
            return false;
        }
        // Never mid-turn: a steer would kill the turn it means to steer.
        let skills = crate::git::claude_skill_args(&self.ws);
        let agent = crate::git::agent_prompt(&self.ws, &self.meta);
        let stale = self.harness_model != self.meta.model
            || self.harness_agent != agent
            || (!self.codex()
                && (self.harness_effort != self.effort() || self.harness_skills != skills));
        if self.harness.is_some() && stale && !self.busy && !self.steered {
            self.harness = None; // drop stops it; the respawn resumes the same session on the new model/level
            self.set_background(vec![], cx); // its reader is replaced below, so its Exited never arrives
        }
        if self.harness.is_none() {
            // A session starting now reads the repos from the root docs and its agent from the
            // system prompt (`git::agent_prompt`), and has nothing else.
            if self.meta.session_id.is_none() {
                let mut context = self.context();
                context.retain(|k, _| {
                    k.starts_with(REPO_CONTEXT) || k.starts_with(crate::state::AGENT_CONTEXT)
                });
                let known = context_record(&context);
                if self.meta.delivered != known {
                    self.meta.delivered = known;
                    cx.emit(ChatEvent::Meta);
                }
            }
            match harness::spawn(&self.ws, &self.meta) {
                Ok((mut a, rx)) => {
                    if self.meta.plan
                        && let Err(e) = a.set_plan_mode(true)
                    {
                        self.push(Block::Info(format!("plan mode failed: {e}")), cx);
                        return false;
                    }
                    self.harness = Some(a);
                    self.harness_model = self.meta.model.clone();
                    self.harness_effort = self.effort();
                    self.harness_skills = skills;
                    self.harness_agent = agent;
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
        // A `/command` must stay the whole message, and a steer lands mid-turn: the context waits
        // for the next normal message.
        let notes = match self.busy || text.trim_start().starts_with('/') {
            true => vec![],
            false => self.pending_context(),
        };
        let wire = match notes.is_empty() {
            true => text.clone(),
            false => format!("{}{NOTES_END}{text}", notes.join("\n")),
        };
        if let Err(e) = self.harness.as_mut().unwrap().send(&wire) {
            self.push(Block::Info(format!("send failed: {e}")), cx);
            return false;
        }
        if !notes.is_empty() {
            self.blocks.extend(info_notes(&notes.join("\n")));
            self.told
                .push((self.blocks.len(), context_record(&self.context()), wire));
        }
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

    /// The agent's repos, branches, issues or attachment changed. Session and Plan mode stay;
    /// the agent hears of it with its next normal turn (`pending_context`). Ending the attachment
    /// hold sends what waited for it, unless a git step still holds it.
    pub fn task_changed(&mut self, ws: &Task, cx: &mut Context<Self>) {
        let grew = ws.repos.len() != self.ws.repos.len();
        let released = self.attach_hold && ws.attaching.is_empty();
        self.ws = ws.clone();
        self.attach_hold = !ws.attaching.is_empty();
        if grew {
            self.reindex(cx);
            self.prs_at = None; // the next `#` lists the new repo's PRs
        }
        if released {
            self.drain(cx);
        }
        cx.notify();
    }

    /// The notes of `Task::context` this chat's session hasn't received, for its next
    /// normal message. Derived from what is saved, so it needs no event: chats closed, not yet
    /// loaded or from before a restart catch up the same way.
    pub(crate) fn pending_context(&self) -> Vec<String> {
        (self.context().into_iter())
            .filter(|(k, note)| self.meta.delivered.get(k) != Some(&note.fingerprint))
            .map(|(_, note)| note.text)
            .collect()
    }

    fn sent(&mut self, text: String, review: Option<Rc<ReviewCard>>, cx: &mut Context<Self>) {
        self.handoff_dismissed = false;
        // codex runs `/compact` as a request of its own, with no user message to echo.
        let echoed = !(self.active_codex() && text.trim() == "/compact");
        let queued = self.busy && echoed;
        if !self.busy {
            self.started = Some(Instant::now());
        }
        self.steered |= queued;
        self.busy = true;
        if self.meta.title.is_empty() {
            self.meta.title = match &review {
                Some(r) => review_title(r),
                None => title_of(&text),
            };
            cx.emit(ChatEvent::Title(review.is_none().then(|| text.clone())));
        }
        // Jump to the end so push follows the new message and the reply.
        self.sel = self.rows.len().saturating_sub(1);
        self.list.set_follow_mode(FollowMode::Tail);
        if echoed {
            self.unread.push_back(self.blocks.len());
        }
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
        let Some(a) = &mut self.harness else {
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
                if let Some(a) = &c.harness
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
        self.harness = None; // drop stops it
        self.drop_asked(cx);
        self.set_background(vec![], cx);
        self.stopping = None;
        self.cut = false;
        self.steered = false;
        self.told.clear(); // never echoed now: that context stays pending
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
            self.handoff_dismissed = false;
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
            Ev::User(text) => self.on_echo(text, cx),
            Ev::Notice(text) => self.blocks.push(Block::Info(text)),
            Ev::TextDelta(s) => self.on_text(s, cx),
            Ev::Text(full) => self.on_full_text(full, cx),
            Ev::ToolUse { id, name, input } => self.on_tool_use(id, name, input),
            Ev::ToolResult { id, text, is_error } => self.on_tool_result(id, text, is_error),
            Ev::Questions(request) => {
                if !self.replaying {
                    self.show_asked(request, cx);
                }
            }
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
                secs,
            } => self.on_result(is_error, text, secs, cx),
            Ev::Exited(msg) => {
                self.drop_process(cx);
                if self.busy {
                    self.busy = false;
                    self.blocks.push(Block::Info(msg));
                    self.keep_reply();
                    self.handoff_dismissed = false;
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
        let stale = !self.replaying && self.active_codex() != self.meta.is_codex();
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
    /// Not just the oldest: one the CLI refused (a failed codex turn/start) is never echoed.
    fn on_echo(&mut self, text: String, cx: &mut Context<Self>) {
        let read =
            self.unread
                .iter()
                .position(|&bi| match self.told.iter().find(|(b, ..)| *b == bi) {
                    Some((_, _, wire)) => wire == &text,
                    None => self.blocks[bi].message().is_some_and(|(t, _)| t == text),
                });
        match read {
            Some(k) => {
                let bi = self.unread.remove(k).unwrap();
                if let Block::User(_, queued) | Block::Review(_, queued) = &mut self.blocks[bi] {
                    *queued = false;
                }
                self.remeasure(bi);
                if let Some(i) = self.told.iter().position(|(b, ..)| *b == bi) {
                    self.meta.delivered = self.told.remove(i).1;
                    cx.emit(ChatEvent::Meta);
                }
                // codex reads a steer into the running turn, with no new init after.
                self.steered = self
                    .unread
                    .iter()
                    .any(|&bi| matches!(self.blocks[bi].message(), Some((_, true))));
            }
            None => {
                // A replayed message carries the task notes sent with it.
                let (notes, text) = split_notes(&text);
                self.blocks.extend(info_notes(notes));
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
                ASK_TOOL => crate::transcript::asked_summary(&input),
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
                && let Some(answer) = crate::transcript::answered(&text)
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
        // Plan mode's approval happens in the plan pane, after the turn: the agent is
        // told the plan was shown, and the turn ends instead of waiting on a person.
        let reason = match tool.as_str() {
            PLAN_TOOL => {
                let text = input["plan"].as_str().unwrap_or_default().to_string();
                self.show_plan(tool_use_id, text, cx);
                Some(PLAN_SHOWN)
            }
            "EnterPlanMode" => Some(ENTER_PLAN),
            // Plan mode asks for these too, and planning needs the web.
            "WebSearch" | "WebFetch" => None,
            _ => Some(NEEDS_APPROVAL),
        };
        if let Some(a) = &mut self.harness
            && let Err(e) = match reason {
                Some(reason) => a.deny(&request_id, reason),
                None => a.allow(&request_id, input),
            }
        {
            self.blocks
                .push(Block::Info(format!("answering the agent failed: {e}")));
        }
    }

    fn on_result(&mut self, is_error: bool, text: String, secs: u64, cx: &mut Context<Self>) {
        self.busy = false;
        if !self.replaying {
            self.stopping = None; // a live interrupt's fallback outlives history
            self.drop_asked(cx);
        }
        let cut = std::mem::take(&mut self.cut);
        // Codex won't start a turn for unread messages after refusal/interruption. Retiring them
        // also keeps a later retry of the same text from acknowledging the refused message.
        if self.active_codex() && (is_error || cut) {
            self.drop_unread(cx);
            self.told.clear();
            self.steered = false;
        }
        // A pruned or deleted session: claude says so in the result and exits, and
        // every resume would fail the same way. (Replay restores the session after.)
        let gone = is_error && text.starts_with(harness::SESSION_GONE);
        let took = duration(secs);
        self.blocks.push(Block::Info(if cut {
            format!("interrupted · {took}")
        } else if is_error {
            format!("error: {text}")
        } else {
            format!("done · {took}")
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
            self.handoff_dismissed = false;
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
                                        .and_then(crate::transcript::answered)
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
                    .child(div().min_w_0().child(t.clone()))
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
        let held = self.held().is_some();
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
            .children(self.render_handoff(cx))
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
                    .on_action(cx.listener(|this, _: &HandOff, _, cx| {
                        if this.show_handoff() {
                            cx.emit(ChatEvent::HandOff { to: this.next_agent.as_ref().map(|a| a.slug.clone()) });
                        }
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
                    .capture_action(cx.listener(|this, _: &crate::BranchFromSource, window, cx| {
                        if this.attach_completion_issue(window, cx) { cx.stop_propagation(); }
                        else if this.unarmed_reference() { this.composer.update(cx, |input, cx| input.replace("\n", window, cx)); cx.stop_propagation(); }
                    }))
                    .capture_action(cx.listener(|this, a: &Enter, window, cx| {
                        // Unarmed reference queries stay text; other Enter presses send.
                        if !a.shift {
                            if this.pick_selected(window, cx) { cx.stop_propagation(); }
                            else if this.unarmed_reference() {
                                this.composer.update(cx, |input, cx| input.replace("\n", window, cx));
                                cx.stop_propagation();
                            }
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
                            .child(self.render_agent_chip(cx))
                            .child(
                                Button::new("plan-mode")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::ClipboardList)
                                    .label("Plan")
                                    .selected(self.meta.plan)
                                    .toggled(self.meta.plan)
                                    .tooltip(if self.meta.plan {
                                        "Plan mode on: the agent plans and edits nothing (shift-tab)"
                                    } else {
                                        "Plan mode: plan first, build after you approve (shift-tab)"
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_plan_mode(!this.meta.plan, cx)
                                    }))
                                    .map(|b| crate::ui::toolbar("plan-mode", b)),
                            )
                            .when(self.codex(), |d| d.child(
                                Button::new("fast-mode")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Zap)
                                    .label("Fast")
                                    .selected(self.fast())
                                    .toggled(self.fast())
                                    .tooltip_with_action(
                                        "Fast: quicker replies that use more of your plan's usage",
                                        &ToggleFastMode,
                                        None,
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_fast(!this.fast(), cx)
                                    }))
                                    .map(|b| crate::ui::toolbar("fast-mode", b)),
                            ))
                            .child(
                                div().relative().min_w_0().child(
                                Button::new("model")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Sparkles)
                                    .label(self.model_chip())
                                    .dropdown_caret(true)
                                    .open(self.model_menu.is_some())
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
                                    })
                                    .map(|b| crate::ui::toolbar("model", b)),
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
                                    .when(self.busy || held, |d| {
                                        d.child(match self.editing {
                                            Some(_) => hint("enter", "save"),
                                            None => hint("enter", "queue"),
                                        })
                                    })
                                    .when(self.busy && !held, |d| {
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
            for ev in harness::parse(l) {
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
                                    Task::default(),
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
                            .map(|i| harness::BackgroundTask {
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
            let mut still = None;
            for state in ["first", "next", "hover", "static", "static-next"] {
                if state == "next" || state == "static-next" {
                    cx.advance_clock(HOP_INTERVAL);
                    cx.run_until_parked();
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
                if state == "static" {
                    // Let the hover-card close delay finish before comparing still frames.
                    cx.advance_clock(std::time::Duration::from_secs(1));
                    cx.run_until_parked();
                }
                if state == "hover" {
                    cx.advance_clock(std::time::Duration::from_secs(1));
                    cx.run_until_parked();
                    cx.update_window(window, |_, w, cx| {
                        w.render_frame(cx);
                        assert!(w.find("activity-details").visible());
                        assert!(w.find(("activity-task", 0usize)).visible());
                    })
                    .unwrap();
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
                if state == "static" {
                    still = Some(shot.clone());
                }
                if state == "static-next" {
                    assert!(
                        still.as_ref().unwrap() == &shot,
                        "reduced motion keeps the critters still"
                    );
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
                .map(|i| harness::BackgroundTask {
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

    /// A claude stand-in that keeps stdin next to it. Side questions and Ask Shark (both
    /// `--no-session-persistence`) get a canned answer, and their args are written next to it.
    const FAKE: &str = r#"#!/bin/sh
case "$*" in *--no-session-persistence*)
  echo "$*" > "$0.args"
  read line
  echo '{"type":"stream_event","event":{"delta":{"type":"text_delta","text":"Forty"}},"parent_tool_use_id":null}'
  echo '{"type":"assistant","message":{"content":[{"type":"text","text":"Forty-two"}]},"parent_tool_use_id":null}'
  echo '{"type":"result","is_error":false,"result":"Forty-two","total_cost_usd":0,"duration_ms":1}'
esac
exec cat >>"$0.stdin"
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
        let ws = Task {
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
        std::fs::create_dir_all(crate::state::tasks_dir()).unwrap();
        let _ = std::fs::remove_file(ws.dir());
        std::os::unix::fs::symlink(&root, ws.dir()).unwrap();
        let (mut a, rx) = harness::spawn_side(&ws, &meta).unwrap();
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
        let (mut a, rx) = harness::spawn_ask("haiku", "the docs").unwrap();
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
        let _restore = Restore(fake.clone());
        // SAFETY: as above.
        unsafe { std::env::set_var("SHARK_CLAUDE", "/nonexistent") };
        let root = std::env::temp_dir().join(format!("shark-noclaude-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let ws = Task {
            name: format!("noclaude-test-{}", std::process::id()),
            ..Default::default()
        };
        let meta = ChatMeta {
            id: ws.name.clone(),
            ..Default::default()
        };
        std::fs::create_dir_all(crate::state::tasks_dir()).unwrap();
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
        // A successful retry consumes the restored draft without sending it twice.
        unsafe { std::env::set_var("SHARK_CLAUDE", &fake) };
        cx.update_window(window, |_, w, cx| {
            chat.update(cx, |c, cx| {
                assert!(c.ask("build the thing", w, cx));
                assert_eq!(c.sent_texts(), ["build the thing"]);
                assert!(c.composer.read(cx).value().is_empty());
                c.reader = None;
            });
        })
        .unwrap();
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
        let ws = Task {
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
        std::fs::create_dir_all(crate::state::tasks_dir()).unwrap();
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
            assert!(side.done && side.harness.is_none() && side.error.is_none());
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
        let ws = Task {
            name: format!("plan-test-{}", std::process::id()),
            repos: vec![crate::state::TaskRepo {
                name: "api".into(),
                src: root.clone(),
                base: "main".into(),
                branch: None,
            }],
            ..Default::default()
        };
        let meta = ChatMeta {
            id: ws.name.clone(),
            ..Default::default()
        };
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::fs::create_dir_all(crate::state::tasks_dir()).unwrap();
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
                            secs: 0,
                        },
                        cx,
                    );
                })
            })
        };
        // The web runs with its input as asked; other tools stay refused.
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                for (id, tool) in [
                    ("web-1", "WebSearch"),
                    ("web-2", "WebFetch"),
                    ("bash-1", "Bash"),
                ] {
                    let input = serde_json::json!({"query": id});
                    let (request_id, tool, tool_use_id) = (id.into(), tool.into(), id.into());
                    c.apply(
                        Ev::Permission {
                            request_id,
                            tool,
                            tool_use_id,
                            input,
                        },
                        cx,
                    );
                }
            })
        });
        let replied = |id: &str| {
            let needle = format!(r#""request_id":"{id}""#);
            let stdin = fake_claude().with_extension("stdin");
            // A cold first run starts the fake slowly.
            for _ in 0..6000 {
                let sent = std::fs::read_to_string(&stdin).unwrap_or_default();
                if let Some(line) = sent.lines().find(|l| l.contains(&needle)) {
                    return serde_json::from_str::<Value>(line).unwrap()["response"]["response"]
                        .clone();
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("no reply to {id}");
        };
        for id in ["web-1", "web-2"] {
            let r = replied(id);
            assert_eq!(r["behavior"], "allow", "{r}");
            assert_eq!(r["updatedInput"]["query"], id, "{r}");
        }
        assert_eq!(replied("bash-1")["message"], NEEDS_APPROVAL);
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
        cx.update(|cx| chat.update(cx, |c, _| c.harness = None));
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
        let ws = Task {
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
                    c.harness_model = c.meta.model.clone(); // a codex process asks
                    c.apply(
                        Ev::Questions(harness::Questions::new(
                            id.into(), format!("call-{id}"), serde_json::json!({"questions": [
                                {"id": "db", "header": "DB", "question": "Which DB?", "isOther": true, "options": [{"label": "Postgres", "description": "SQL"}, {"label": "Redis", "description": "KV"}]},
                                {"id": "tests", "header": "Tests", "question": "Which tests?", "isOther": true, "options": [{"label": "Unit", "description": ""}, {"label": "E2E", "description": ""}]},
                            ]}), true,
                        )),
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
        let ws = Task {
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

    /// A chat on a fresh task resuming session `s1`.
    fn open_chat(tag: &str, cx: &mut TestAppContext) -> Entity<Chat> {
        cx.update(gpui_kit::init);
        let ws = Task {
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

    /// Toolbar controls keep their hit targets, keyboard activation and model-menu focus
    /// when the composer has little room or the app changes theme.
    #[gpui_kit::test]
    fn composer_toolbar_interactions_and_layout(cx: &mut TestAppContext) {
        struct ComposerControls(Entity<Chat>);
        impl Render for ComposerControls {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div()
                    .size_full()
                    .tab_group()
                    .on_action(cx.listener(|s, _: &PickModel, w, cx| {
                        s.0.update(cx, |c, cx| c.open_model_menu(w, cx));
                    }))
                    .child(self.0.clone())
            }
        }

        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        for mode in [crate::state::Mode::Dark, crate::state::Mode::Light] {
            for width in [420., 800.] {
                let (window, host) = cx
                    .update(|cx| {
                        crate::theme::apply(
                            &crate::state::Appearance {
                                mode,
                                ..Default::default()
                            },
                            cx,
                        );
                        gpui_kit::open_window(
                            WindowOptions {
                                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                    point(px(0.), px(0.)),
                                    size(px(width), px(500.)),
                                ))),
                                ..Default::default()
                            },
                            cx,
                            |w, cx| {
                                let fixture =
                                    format!("composer-toolbar-{}-{width}", std::process::id());
                                let chat = cx.new(|cx| {
                                    let mut chat = Chat::new(
                                        Task {
                                            name: fixture.clone(),
                                            ..Default::default()
                                        },
                                        ChatMeta {
                                            id: fixture,
                                            model: Some("codex:gpt-6.1-sol".into()),
                                            fast: Some(false),
                                            ..Default::default()
                                        },
                                        false,
                                        w,
                                        cx,
                                    );
                                    chat.plan_open = false;
                                    chat
                                });
                                cx.new(|_| ComposerControls(chat))
                            },
                        )
                    })
                    .unwrap();
                cx.update_window(window, |_, w, cx| {
                    w.render_frame(cx);
                    let plan = w.find("plan-mode");
                    let fast = w.find("fast-mode");
                    let model = w.find("model");
                    assert_eq!(plan.role(), Some(Role::Button));
                    assert_eq!(fast.role(), Some(Role::Button));
                    for button in [&plan, &fast, &model] {
                        assert!(button.visible());
                        assert_eq!(button.bounds().size.height, px(32.));
                        assert!(button.bounds().left() >= px(16.));
                        assert!(button.bounds().right() <= px(width - 16.));
                        assert_eq!(button.bounds().top(), plan.bounds().top());
                    }
                    assert!(plan.bounds().right() <= fast.bounds().left());
                    assert!(fast.bounds().right() <= model.bounds().left());
                    w.click("fast-mode", cx);
                    let chat = host.read(cx).0.clone();
                    assert!(chat.read(cx).fast(), "click enables Fast");
                    for _ in 0..6 {
                        w.focus_next(cx);
                        w.render_frame(cx);
                        if w.find("fast-mode").focused() == Some(true) {
                            break;
                        }
                    }
                    assert_eq!(w.find("fast-mode").focused(), Some(true));
                    w.press("space", cx);
                    assert!(!chat.read(cx).fast(), "Space toggles the focused button");
                    w.focus_next(cx);
                    w.render_frame(cx);
                    assert_eq!(w.find("model").focused(), Some(true));
                    w.click("model", cx);
                })
                .unwrap();
                cx.run_until_parked();
                cx.update_window(window, |_, w, cx| {
                    w.render_frame(cx);
                    let chat = host.read(cx).0.clone();
                    assert!(chat.read(cx).model_menu.is_some());
                    assert!(w.find("menu-parent").visible());
                    w.press("escape", cx);
                })
                .unwrap();
                cx.run_until_parked();
                cx.update_window(window, |_, w, cx| {
                    w.render_frame(cx);
                    let chat = host.read(cx).0.clone();
                    assert!(
                        chat.read(cx).model_menu.is_none(),
                        "Escape closes the model menu before moving to the transcript (transcript focused: {})",
                        chat.read(cx).focus.is_focused(w)
                    );
                    assert_eq!(w.find("model").focused(), Some(true));
                    w.remove_window();
                })
                .unwrap();
            }
        }
    }

    struct InfoPreview(Entity<Chat>);

    impl Render for InfoPreview {
        fn render(&mut self, w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .bg(cx.theme().background)
                .child(self.0.update(cx, |c, cx| c.render_row(1, w, cx)))
        }
    }

    fn info_preview(w: &mut Window, cx: &mut App) -> Entity<InfoPreview> {
        let chat = cx.new(|cx| Chat::new(Task::default(), ChatMeta::default(), false, w, cx));
        chat.update(cx, |c, cx| {
            c.apply(Ev::Text("**reply**".into()), cx);
            c.push(Block::Info("done · 1s".into()), cx);
        });
        cx.new(|_| InfoPreview(chat))
    }

    fn check_info_layout(host: &Entity<InfoPreview>, w: &mut Window, cx: &mut App, width: f32) {
        let chat = host.read(cx).0.clone();
        let mut short_copy_top = px(0.);
        for (i, text) in [
            "done · 1s".to_string(),
            "[shark] Repository instructions could not be loaded from the requested working directory. ".repeat(8),
            format!("[shark] instructions: /Users/{}AGENTS.md", "very_long_directory_name/".repeat(16)),
        ].into_iter().enumerate() {
            chat.update(cx, |c, cx| {
                c.blocks[1] = Block::Info(text);
                cx.notify();
            });
            w.render_frame(cx);
            let copy = w.find(("copy", 1usize));
            assert!(copy.visible());
            assert!(copy.bounds().left() >= w.rem_size());
            assert!(copy.bounds().right() <= w.viewport_size().width - w.rem_size(), "case {i}, width {width}, copy {:?}", copy.bounds());
            if i == 0 {
                short_copy_top = copy.bounds().top();
                assert!(copy.bounds().left() < px(width / 2.), "short status keeps copy adjacent");
            } else {
                assert!(copy.bounds().top() > short_copy_top + px(12.), "long Info wraps: case {i}, width {width}, copy {:?}", copy.bounds());
            }
        }
    }

    #[gpui_kit::test]
    fn info_rows_wrap_and_keep_copy_reachable(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        for width in [320., 420., 800.] {
            let (window, host) = cx
                .update(|cx| {
                    gpui_kit::open_window(
                        WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                point(px(0.), px(0.)),
                                size(px(width), px(700.)),
                            ))),
                            ..Default::default()
                        },
                        cx,
                        info_preview,
                    )
                })
                .unwrap();
            cx.update_window(window, |_, w, cx| check_info_layout(&host, w, cx, width))
                .unwrap();
        }
    }

    /// The composer's agent button with no agent, with one, and once the chat started, in both
    /// modes; `SHARK_RENDER_DIR` keeps the screenshots.
    pub(crate) fn render_agent_chip_pixels(cx: &mut HeadlessAppContext) {
        let ws = Task {
            name: "agent-chip".into(),
            agents: vec![crate::state::AgentLink {
                slug: "designer".into(),
                name: "Designer".into(),
                icon: crate::state::AgentIcon::Emoji {
                    value: "🎨".into()
                },
                ..Default::default()
            }],
            ..Default::default()
        };
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
                )
            });
            for case in ["none", "agent", "started"] {
                let ws = ws.clone();
                let (window, chat) = cx
                    .update(|cx| {
                        gpui_kit::open_window(
                            WindowOptions {
                                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                    point(px(0.), px(0.)),
                                    size(px(760.), px(260.)),
                                ))),
                                focus: false,
                                show: false,
                                ..Default::default()
                            },
                            cx,
                            move |w, cx| {
                                let meta = ChatMeta {
                                    agent: (case != "none").then(|| "designer".into()),
                                    model: Some(crate::models::MODELS[0].into()),
                                    ..Default::default()
                                };
                                cx.new(|cx| Chat::new(ws, meta, false, w, cx))
                            },
                        )
                    })
                    .unwrap();
                cx.update_window(window, |_, w, cx| {
                    chat.update(cx, |c, cx| {
                        if case == "started" {
                            c.apply(Ev::User("Sketch the onboarding".into()), cx);
                            c.apply(Ev::Text("Here are three directions.".into()), cx);
                        }
                    });
                    w.render_frame(cx);
                    let chip = w.find("chat-agent");
                    let model = w.find("model");
                    assert!(chip.visible() && model.visible(), "{case}");
                    assert!(
                        chip.bounds().right() <= model.bounds().left(),
                        "agent comes first"
                    );
                    assert_eq!(chip.bounds().top(), model.bounds().top(), "one row");
                })
                .unwrap();
                if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
                    let dir = PathBuf::from(dir);
                    std::fs::create_dir_all(&dir).unwrap();
                    cx.capture_screenshot(window)
                        .expect("real agent chip renderer")
                        .save(dir.join(format!("agent-chip-{label}-{case}.png")))
                        .unwrap();
                }
                cx.update_window(window, |_, w, _| w.remove_window())
                    .unwrap();
            }
        }
    }

    pub(crate) fn render_info_pixels(cx: &mut HeadlessAppContext) {
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
                )
            });
            for width in [320., 420., 800.] {
                let (window, host) = cx
                    .update(|cx| {
                        gpui_kit::open_window(
                            WindowOptions {
                                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                    point(px(0.), px(0.)),
                                    size(px(width), px(700.)),
                                ))),
                                focus: false,
                                show: false,
                                ..Default::default()
                            },
                            cx,
                            info_preview,
                        )
                    })
                    .unwrap();
                cx.update_window(window, |_, w, cx| check_info_layout(&host, w, cx, width))
                    .unwrap();
                for copy in [true, false] {
                    if !copy {
                        cx.update_window(window, |_, w, cx| {
                            host.read(cx).0.clone().update(cx, |c, cx| {
                                c.blocks[0] = Block::Info("[shark] instructions".into());
                                cx.notify();
                            });
                            w.render_frame(cx);
                            assert!(w.find_all(("copy", 1usize)).is_empty());
                        })
                        .unwrap();
                    }
                    if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
                        let dir = PathBuf::from(dir);
                        std::fs::create_dir_all(&dir).unwrap();
                        cx.capture_screenshot(window)
                            .expect("real Info renderer")
                            .save(
                                dir.join(format!(
                                    "info-{label}-{}-copy-{copy}.png",
                                    width as usize
                                )),
                            )
                            .unwrap();
                    }
                }
                cx.update_window(window, |_, w, _| w.remove_window())
                    .unwrap();
            }
        }
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

    #[gpui_kit::test]
    fn streaming_mermaid_reply_renders_after_fence_closes(cx: &mut TestAppContext) {
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::state::TEST_HOME
            .get_or_init(|| std::env::temp_dir().join(format!("shark-ui-{}", std::process::id())));
        let chat = open_chat("mermaid-stream", cx);
        let window = cx.windows()[0];
        let open = "```mermaid\nflowchart TD\n  A[Start] --> B[Done]\n";
        let closed = format!("{open}```");
        cx.update(|cx| {
            chat.update(cx, |c, cx| c.apply(Ev::TextDelta(open.into()), cx));
        });
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            assert!(
                w.try_find("md-mermaid").is_none(),
                "open fence stays source"
            );
        })
        .unwrap();
        cx.update(|cx| {
            chat.update(cx, |c, cx| c.apply(Ev::Text(closed.clone()), cx));
        });
        cx.update_window(window, |_, w, cx| w.render_frame(cx))
            .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            let bounds = w.find("md-mermaid").bounds();
            assert!(
                bounds.size.height > px(20.) && bounds.size.height <= px(560.),
                "{bounds:?}"
            );
            chat.update(cx, |c, cx| {
                assert_eq!(c.blocks.len(), 1, "streaming updates keep one reply");
                assert!(matches!(&c.blocks[0], Block::Text { src, .. } if src == &closed));
                c.sel = 0;
                c.copy_selected(w, cx);
            });
        })
        .unwrap();
        assert_eq!(
            cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text())),
            Some(closed),
            "copy preserves the Mermaid source"
        );
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
                c.harness_model = Some("codex:gpt-6.1-sol".into());
                let init =
                    r#"{"method":"turn/started","params":{"threadId":"th1","turn":{"id":"t1"}}}"#;
                for ev in harness::parse(init) {
                    c.apply(ev, cx);
                }
                assert_eq!(c.meta.session_id, None);
            })
        });
    }

    /// A model pick affects the successor, while compact and failed steers keep the live backend's semantics.
    #[gpui_kit::test]
    fn busy_backend_switch_keeps_live_compact_and_result_semantics(cx: &mut TestAppContext) {
        use std::os::unix::fs::PermissionsExt;
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root =
            std::env::temp_dir().join(format!("shark-backend-switch-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let fake = root.join("agent");
        std::fs::write(&fake, r#"#!/bin/sh
: > "$0.in"
while IFS= read -r line; do
    printf '%s\n' "$line" >> "$0.in"
    case "$line" in
        *'"thread/resume"'*) printf '%s\n' '{"id":"resume","result":{"thread":{"id":"th1"},"model":"m1"}}';;
        *'"thread/start"'*) printf '%s\n' '{"id":"start","result":{"thread":{"id":"th1"},"model":"m1"}}';;
    esac
done
"#).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        struct Restore([Option<std::ffi::OsString>; 2]);
        impl Drop for Restore {
            fn drop(&mut self) {
                for (key, old) in ["SHARK_CLAUDE", "SHARK_CODEX"].into_iter().zip(&self.0) {
                    // SAFETY: all provider-spawning tests hold UI_LOCK.
                    unsafe {
                        match old {
                            Some(old) => std::env::set_var(key, old),
                            None => std::env::remove_var(key),
                        }
                    }
                }
            }
        }
        let _restore = Restore([
            std::env::var_os("SHARK_CLAUDE"),
            std::env::var_os("SHARK_CODEX"),
        ]);
        for key in ["SHARK_CLAUDE", "SHARK_CODEX"] {
            // SAFETY: as above.
            unsafe { std::env::set_var(key, &fake) };
        }
        for (running, selected, is_codex) in [
            ("codex:gpt-6.1-sol", "opus", true),
            ("opus", "codex:gpt-6.1-sol", false),
        ] {
            let chat = open_chat(
                if is_codex {
                    "switch-from-codex"
                } else {
                    "switch-from-claude"
                },
                cx,
            );
            chat.update(cx, |c, cx| {
                std::fs::create_dir_all(c.ws.dir()).unwrap();
                c.meta.model = Some(running.into());
                assert!(c.send_text("first".into(), false, cx));
                c.apply(Ev::User("first".into()), cx);
                c.set_model(selected, cx);
                assert_eq!(c.harness.as_ref().unwrap().is_codex(), is_codex);
                assert_eq!(c.active_codex(), is_codex);
                assert!(c.send_text("/compact".into(), true, cx));
                assert_eq!(c.unread.len(), usize::from(!is_codex));
                assert_eq!(c.steered, !is_codex);
                let compact_seen = || {
                    std::fs::read_to_string(fake.with_extension("in"))
                        .unwrap_or_default()
                        .lines()
                        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                        .any(|v| {
                            if is_codex {
                                v["method"] == "thread/compact/start"
                            } else {
                                v["type"] == "user" && v["message"]["content"] == "/compact"
                            }
                        })
                };
                for _ in 0..1000 {
                    if compact_seen() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                assert!(
                    compact_seen(),
                    "compact must reach the process that is still running"
                );
                // Keep transport events out of the manually driven result assertions.
                c.reader = None;
                c.unread.clear();
                c.steered = false;
                for interrupted in [false, true] {
                    c.busy = true;
                    let wire = "pending".to_string();
                    c.told.push((
                        c.blocks.len(),
                        [("issue:one".into(), 1)].into_iter().collect(),
                        wire.clone(),
                    ));
                    c.sent(wire, None, cx);
                    if interrupted {
                        c.apply(Ev::Interrupted, cx);
                    }
                    c.apply(
                        Ev::Result {
                            is_error: !interrupted,
                            text: "refused".into(),
                            secs: 0,
                        },
                        cx,
                    );
                    assert_eq!(
                        c.unread.is_empty(),
                        is_codex,
                        "interrupted={interrupted}, running={running}"
                    );
                    assert_eq!(c.told.is_empty(), is_codex);
                    assert_eq!(c.steered, !is_codex);
                    c.unread.clear();
                    c.told.clear();
                    c.steered = false;
                }
                c.drop_process(cx);
                std::fs::remove_dir_all(c.ws.dir()).unwrap();
            });
        }
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
                c.harness_model = c.meta.model.clone();
                c.harness_effort = "high";
                assert_ne!(c.harness_effort, c.effort());
            })
        });
    }

    #[gpui_kit::test]
    fn added_repo_skills_restart_claude_only_on_idle_send(cx: &mut TestAppContext) {
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        fake_claude();
        let chat = open_chat("added-repo-skills", cx);
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                let dir = c.ws.dir();
                std::fs::create_dir_all(&dir).unwrap();
                assert!(c.send_text("first".into(), false, cx));
                assert!(c.harness_skills.is_empty());
                let mut ws = c.ws.clone();
                std::fs::create_dir_all(dir.join("api")).unwrap();
                ws.repos.push(crate::state::TaskRepo {
                    name: "api".into(),
                    src: dir.join("api"),
                    base: "HEAD".into(),
                    branch: None,
                });
                c.task_changed(&ws, cx);
                assert!(c.send_text("steer".into(), true, cx));
                assert!(c.harness_skills.is_empty(), "never restart an active turn");
                c.apply(Ev::User("first".into()), cx);
                c.apply(Ev::User("steer".into()), cx);
                c.apply(
                    Ev::Result {
                        is_error: false,
                        text: String::new(),
                        secs: 0,
                    },
                    cx,
                );
                assert!(c.send_text("next".into(), false, cx));
                assert_eq!(c.harness_skills, crate::git::claude_skill_args(&ws));
                assert_eq!(c.harness_skills.len(), 2, "restarted with the added repo");
                c.reader = None;
                c.harness = None;
                std::fs::remove_dir_all(dir).unwrap();
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
                c.harness_model = c.meta.model.clone();
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

    /// A replayed message that went out with task notes shows them as Info rows, not in
    /// the user's bubble.
    #[gpui_kit::test]
    fn replay_splits_task_notes_from_the_message(cx: &mut TestAppContext) {
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

    /// Context notes may hold blank lines, and what they quote stays out of the rows and the
    /// user's bubble.
    #[gpui_kit::test]
    fn replay_keeps_multi_paragraph_notes_out_of_the_message(cx: &mut TestAppContext) {
        let chat = open_chat("note-paragraphs", cx);
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                c.replaying = true;
                let wire = format!(
                    "[shark] Issue ENG-1 changed:\nFirst paragraph.\n\nSecond.{NOTES_END}fix it"
                );
                c.apply(Ev::User(wire), cx);
                c.replaying = false;
                assert!(matches!(&c.blocks[0], Block::Info(t) if t.ends_with("changed:")));
                assert!(matches!(&c.blocks[1], Block::User(t, _) if t == "fix it"));
                assert_eq!(c.blocks.len(), 2);
            })
        });
    }

    #[gpui_kit::test]
    fn agent_notes_are_per_chat_and_its_pick_locks_once_started(cx: &mut TestAppContext) {
        let chat = open_chat("chat-agent", cx);
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                c.meta.session_id = None;
                c.ws.agents = vec![crate::state::AgentLink {
                    slug: "planner".into(),
                    name: "Planner".into(),
                    instructions: "Plan carefully".into(),
                    ..Default::default()
                }];
                assert!(
                    !c.context().contains_key("agent:planner"),
                    "another chat's agent"
                );
                assert!(crate::git::agent_prompt(&c.ws, &c.meta).is_none());
                let defaults = Some((crate::models::MODELS[1].to_string(), Some("low".into())));
                assert!(c.set_agent(Some("planner".into()), defaults, cx));
                assert_eq!(c.meta.model.as_deref(), Some(crate::models::MODELS[1]));
                assert_eq!(c.meta.effort.as_deref(), Some("low"));
                let notes = c.context();
                assert!(notes.contains_key("agent:planner"));
                let prompt = crate::git::agent_prompt(&c.ws, &c.meta).unwrap();
                assert!(prompt.contains("Planner") && prompt.contains("Plan carefully"));
                assert!(!c.set_agent(Some("planner".into()), None, cx), "unchanged");
                c.blocks.push(Block::User("Go".into(), false));
                assert!(!c.agent_open());
                assert!(
                    !c.set_agent(None, None, cx),
                    "a started chat keeps its agent"
                );
                assert_eq!(c.meta.agent.as_deref(), Some("planner"));
            })
        });
    }

    /// A message the agent never read leaves its context pending: only an echo records it.
    #[gpui_kit::test]
    fn context_stays_pending_until_the_agent_reads_it(cx: &mut TestAppContext) {
        let chat = open_chat("context-echo", cx);
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                c.ws.repos.push(crate::state::TaskRepo {
                    name: "api".into(),
                    src: "/src/api".into(),
                    base: "main".into(),
                    branch: None,
                });
                let record = context_record(&c.ws.context());
                let unread = |c: &mut Chat, record: &std::collections::BTreeMap<_, _>| {
                    c.blocks.push(Block::User("hi".into(), false));
                    c.unread.push_back(c.blocks.len() - 1);
                    c.told.push((
                        c.blocks.len() - 1,
                        record.clone(),
                        "[shark] x\n[/shark]\n\nhi".into(),
                    ));
                };
                unread(c, &record);
                c.apply(Ev::Exited("claude exited".into()), cx);
                assert!(c.told.is_empty() && c.meta.delivered.is_empty());
                assert_eq!(c.pending_context().len(), 1);
                unread(c, &record);
                c.apply(Ev::User("[shark] x\n[/shark]\n\nhi".into()), cx);
                assert_eq!(c.meta.delivered, record);
                assert!(c.pending_context().is_empty());
            })
        });
    }

    /// The real Codex transport carries repo and issue notes, leaves compact requests alone,
    /// and acknowledges context only when its app-server echoes the user message.
    #[gpui_kit::test]
    fn codex_delivers_attachment_context_on_normal_turns(cx: &mut TestAppContext) {
        use std::os::unix::fs::PermissionsExt;
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let chat = open_chat("codex-context", cx);
        let root = std::env::temp_dir().join(format!("shark-codex-context-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let fake = root.join("codex");
        std::fs::write(&fake, r#"#!/bin/sh
: > "$0.in"
while IFS= read -r line; do
    printf '%s\n' "$line" >> "$0.in"
    case "$line" in
        *'"thread/resume"'*) printf '%s\n' '{"id":"resume","result":{"thread":{"id":"th1"},"model":"m1"}}';;
        *'"thread/start"'*) printf '%s\n' '{"id":"start","result":{"thread":{"id":"th1"},"model":"m1"}}';;
    esac
done
"#).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        struct Restore(Option<std::ffi::OsString>);
        impl Drop for Restore {
            fn drop(&mut self) {
                // SAFETY: tests that spawn providers hold UI_LOCK.
                unsafe {
                    match &self.0 {
                        Some(old) => std::env::set_var("SHARK_CODEX", old),
                        None => std::env::remove_var("SHARK_CODEX"),
                    }
                }
            }
        }
        let _restore = Restore(std::env::var_os("SHARK_CODEX"));
        // SAFETY: as above.
        unsafe { std::env::set_var("SHARK_CODEX", &fake) };
        let request = |n: usize| {
            for _ in 0..1000 {
                let log = std::fs::read_to_string(fake.with_extension("in")).unwrap_or_default();
                let turns: Vec<Value> = log
                    .lines()
                    .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                    .filter(|v| {
                        matches!(
                            v["method"].as_str(),
                            Some("turn/start" | "thread/compact/start")
                        )
                    })
                    .collect();
                if let Some(turn) = turns.get(n) {
                    return turn.clone();
                }
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
            panic!("Codex request {n} was not received");
        };
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                std::fs::create_dir_all(c.ws.dir()).unwrap();
                c.meta.model = Some("codex:gpt-6.1-sol".into());
                c.ws.repos.push(crate::state::TaskRepo { name: "api".into(), src: "/src/api".into(), base: "main".into(), branch: None });
                let issue = crate::linear::Snapshot { org: "org".into(), id: "one".into(), identifier: "ENG-1".into(), title: "Fix login".into(), description: "Use the existing session".into(), ..Default::default() };
                c.ws.linear.attach((&issue).into());
                c.ws.issues.push(issue);
                assert!(c.send_text("/compact".into(), false, cx));
                assert_eq!(request(0)["method"], "thread/compact/start");
                assert_eq!(c.pending_context().len(), 2);
                c.play(Ev::Result { is_error: false, text: String::new(), secs: 0 }, cx);
                assert!(c.send_text("build it".into(), false, cx));
                let wire = request(1)["params"]["input"][0]["text"].as_str().unwrap().to_string();
                assert!(wire.contains("Repo `api`") && wire.contains("Linear issue ENG-1"), "{wire}");
                assert!(wire.ends_with("build it"));
                assert_eq!(c.pending_context().len(), 2, "not yet echoed");
                let echo = serde_json::json!({"method":"item/started", "params":{"item":{"type":"userMessage", "id":"u1", "content":[{"type":"text", "text":wire}]}}});
                play(c, &[&echo.to_string()], cx);
                assert!(c.pending_context().is_empty());
                c.play(Ev::Result { is_error: false, text: String::new(), secs: 0 }, cx);
                assert!(c.send_text("next".into(), false, cx));
                assert_eq!(request(2)["params"]["input"][0]["text"], "next");
                c.drop_process(cx);
                let _ = std::fs::remove_dir_all(c.ws.dir());
            })
        });
    }

    /// A refused message cannot claim another message's echo, even with the same user text
    /// after its context changed.
    #[gpui_kit::test]
    fn echoes_match_the_exact_message_and_context(cx: &mut TestAppContext) {
        let chat = open_chat("exact-echo", cx);
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                c.sent("it".into(), None, cx);
                c.busy = false; // refused without an echo
                c.sent("fix it".into(), None, cx);
                c.apply(Ev::User("fix it".into()), cx);
                assert_eq!(c.unread.iter().copied().collect::<Vec<_>>(), [0]);

                let old = [("issue:one".into(), 1)].into_iter().collect();
                let new: std::collections::BTreeMap<String, u64> =
                    [("issue:one".into(), 2)].into_iter().collect();
                for (record, wire) in [
                    (old, format!("[shark] Old context{NOTES_END}retry")),
                    (new.clone(), format!("[shark] New context{NOTES_END}retry")),
                ] {
                    c.busy = false;
                    c.told.push((c.blocks.len(), record, wire));
                    c.sent("retry".into(), None, cx);
                }
                c.apply(Ev::User(format!("[shark] New context{NOTES_END}retry")), cx);
                assert_eq!(c.meta.delivered, new);
                assert_eq!(
                    c.told.len(),
                    1,
                    "the refused context was never acknowledged"
                );
                assert_eq!(c.unread.len(), 2, "only the matching message was read");
            })
        });
    }

    /// History can finish loading between a turn ending and a pending steer being read.
    #[gpui_kit::test]
    fn history_preserves_pending_steers_and_context(cx: &mut TestAppContext) {
        let chat = open_chat("history-steer", cx);
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                c.sent("first".into(), None, cx);
                c.apply(Ev::User("first".into()), cx);
                let wire = format!("[shark] Context{NOTES_END}steer");
                let record = [("issue:one".into(), 1)].into_iter().collect();
                c.told.push((c.blocks.len(), record, wire.clone()));
                c.sent("steer".into(), None, cx);
                c.apply(
                    Ev::Result {
                        is_error: false,
                        text: String::new(),
                        secs: 1,
                    },
                    cx,
                );
                assert!(!c.idle());
                c.load_history(
                    vec![
                        Ev::User("history".into()),
                        Ev::Init {
                            session_id: "old".into(),
                            model: "m".into(),
                            permission_mode: "plan".into(),
                            mcp: vec![],
                        },
                        Ev::Exited("old process exited".into()),
                    ],
                    cx,
                );
                assert!(
                    !c.idle(),
                    "attachments and deletion must still wait for the steer"
                );
                assert_eq!(c.told.len(), 1);
                c.apply(Ev::User(wire), cx);
                assert_eq!(c.meta.delivered.get("issue:one"), Some(&1));
                assert!(c.idle());
            })
        });
    }

    /// While an attachment holds a chat, every way of sending stops at the same gate and nothing
    /// is lost; a git hold ending doesn't change that. Plan mode and the plan stay as they are.
    #[gpui_kit::test]
    fn attach_hold_stops_every_way_of_sending(cx: &mut TestAppContext) {
        let chat = open_chat("attach-hold", cx);
        let window = cx.windows()[0];
        let act = |cx: &mut TestAppContext,
                   f: &dyn Fn(&mut Chat, &mut Window, &mut Context<Chat>)| {
            cx.update_window(window, |_, w, cx| chat.update(cx, |c, cx| f(c, w, cx)))
                .unwrap();
        };
        act(cx, &|c, w, cx| {
            c.attach_hold = true;
            c.ws.repos.push(crate::state::TaskRepo {
                name: "api".into(),
                src: "/src/api".into(),
                base: "main".into(),
                branch: None,
            });
            c.ws.attaching.push(crate::state::Attachment::fresh(
                &crate::state::Repo {
                    name: "web".into(),
                    path: "/src/web".into(),
                },
                "b",
                "",
            ));
            c.meta.plan = true;
            c.plan
                .update(cx, |p, cx| p.add("p1".into(), "# Plan".into(), cx));
            assert!(!c.send_text("direct".into(), false, cx), "the gate itself");
            assert!(!c.ask("host prompt", w, cx), "ask");
            c.approve_plan(w, cx);
            assert!(!c.build_plan("# Plan", w, cx), "build");
            c.set_plan_mode(false, cx);
            assert!(c.meta.plan, "attachment never leaves Plan mode");
            assert_eq!(c.plan.read(cx).approved, None, "or approves");
            // Composer, git prompt and review: they queue.
            c.composer.update(cx, |t, cx| t.set_value("typed", w, cx));
            c.send(false, w, cx);
            c.composer.update(cx, |t, cx| t.set_value("now!", w, cx));
            c.send(true, w, cx);
            assert!(c.enqueue("git prompt", vec![], cx));
            let card = ReviewCard {
                prompt: "R".into(),
                items: vec![],
                changes: String::new(),
            };
            assert!(c.send_review(card, cx));
            assert_eq!(c.queued(), 4);
            c.qsel = 1;
            c.steer(w, cx);
            c.drain(cx);
            assert_eq!(c.queued(), 4, "steering and draining wait");
            assert!(c.sent_texts().is_empty());
            assert!(c.harness.is_none(), "no provider started");
            // A git step coming and going leaves it alone.
            c.hold = true;
            c.hold = false;
            c.drain(cx);
            assert_eq!(c.queued(), 4);
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
                c.harness_model = c.meta.model.clone();
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

    /// Entries codex will never echo don't keep a chat from going idle: `/compact` isn't recorded,
    /// a refused message doesn't outlive the turn it was refused in, and a steer read later isn't
    /// held up by either. An attachment waiting on `idle` would hold the turn that clears them.
    #[gpui_kit::test]
    fn stale_codex_messages_leave_the_chat_idle(cx: &mut TestAppContext) {
        let chat = open_chat("codex-stale", cx);
        let done = || Ev::Result {
            is_error: false,
            text: String::new(),
            secs: 1,
        };
        let refused = || Ev::Result {
            is_error: true,
            text: "bad input".into(),
            secs: 0,
        };
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                c.meta.model = Some("codex:gpt-6-astra".into());
                c.harness_model = c.meta.model.clone();
                c.sent("/compact".into(), None, cx);
                assert!(c.unread.is_empty(), "never echoed");
                c.apply(done(), cx);
                c.sent("refused".into(), None, cx);
                c.apply(refused(), cx);
                assert!(c.unread.is_empty());

                // A steer read with those around, as the reviewer's case had it.
                c.sent("turn".into(), None, cx);
                c.apply(Ev::User("turn".into()), cx);
                c.sent("/compact".into(), None, cx);
                assert!(!c.steered, "no steer to read");
                c.sent("steer".into(), None, cx);
                assert!(c.steered);
                c.apply(Ev::User("steer".into()), cx);
                assert!(!c.steered, "the stale entry doesn't hold it");
                c.apply(done(), cx);
                assert!(c.idle());

                // Retrying refused text as a steer must read the retry, not the stale entry.
                c.sent("again".into(), None, cx);
                c.apply(refused(), cx);
                c.sent("turn".into(), None, cx);
                c.apply(Ev::User("turn".into()), cx);
                c.sent("again".into(), None, cx);
                c.apply(Ev::User("again".into()), cx);
                c.apply(done(), cx);
                assert!(c.idle() && c.unread.is_empty());

                // A steer refused, then one in an interrupted turn: nothing starts a turn for them.
                c.sent("turn".into(), None, cx);
                c.apply(Ev::User("turn".into()), cx);
                c.sent("steer 2".into(), None, cx);
                c.apply(refused(), cx);
                assert!(c.idle(), "refused");
                c.sent("turn".into(), None, cx);
                c.apply(Ev::User("turn".into()), cx);
                c.sent("steer 3".into(), None, cx);
                c.apply(Ev::Interrupted, cx);
                c.apply(done(), cx);
                assert!(c.idle(), "interrupted");

                // A steer left unread by a turn that ended well does start its own: still waits.
                c.sent("turn".into(), None, cx);
                c.apply(Ev::User("turn".into()), cx);
                c.sent("steer 4".into(), None, cx);
                c.apply(done(), cx);
                assert!(!c.idle());
                c.apply(Ev::User("steer 4".into()), cx);
                assert!(c.idle());
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
                    secs: 3,
                };
                c.apply(result, cx);
                let Some(Block::Info(t)) = c.blocks.last() else {
                    panic!()
                };
                assert_eq!(t, "interrupted · 3s");
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
        let evs = || lines.iter().flat_map(|l| crate::harness::parse(l));
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
                crate::harness::parse(echo).into_iter().for_each(|e| c.apply(e, cx));
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
        let ws = Task {
            name: format!("queue-test-{}", std::process::id()),
            branch: "b".into(),
            repos: vec![],
            ..Default::default()
        };
        // The UI tests reset the test home; don't let one do that under this task.
        let meta = ChatMeta {
            id: ws.name.clone(),
            ..Default::default()
        };
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::fs::create_dir_all(crate::state::tasks_dir()).unwrap();
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
        cx.update(|cx| chat.update(cx, |c, _| c.harness = None));
        let _ = std::fs::remove_file(dir);
        let _ = std::fs::remove_file(transcript);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// `@` opens the file menu; keys drive it and a pick leaves a repo-qualified chip.
    #[gpui_kit::test]
    fn dropping_a_file_attaches_it(cx: &mut TestAppContext) {
        // Its task dir is under the UI tests' home, which they wipe.
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::bind_keys(cx);
        });
        let ws = Task {
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
        let repo = |n: &str| crate::state::TaskRepo {
            name: n.into(),
            src: root.join(n),
            base: "origin/main".into(),
            branch: None,
        };
        let ws = Task {
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
                c.indexing = Some(gpui_kit::Task::ready(())); // the workspace dir doesn't exist; keep these
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
        for key in ["enter", "alt-enter"] {
            act(cx, &|w, cx| w.input(" #endif", cx));
            assert_eq!(armed(cx), Some(false));
            act(cx, &|w, cx| w.press(key, cx));
            assert!(value(cx).ends_with(" #endif\n"), "{key}: {}", value(cx));
            cx.update(|cx| {
                assert!(
                    chat.read(cx).queue.is_empty(),
                    "unarmed references never submit"
                )
            });
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui_kit::test]
    fn vim_in_the_composer(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::bind_keys(cx);
        });
        let ws = Task {
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
