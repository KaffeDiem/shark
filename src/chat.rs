//! One agent conversation (a chat tab): transcript blocks, composer, and the claude process.
mod completion;
#[cfg(all(test, feature = "test-support"))]
pub(crate) mod completion_pixels;
mod question;
mod side;

use crate::agent::{self, Agent, Ev};
use crate::complete::{self, Item};
use crate::plan::{self, PlanPane};
use crate::state::{ChatMeta, Workspace};
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
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::shimmer::ShimmerText;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::{TextView, TextViewState, TextViewStyle};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon, Selectable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;
use side::Side;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
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

/// Diff comments sent as one message (src/app/review.rs), shown as a card instead of the
/// prompt text.
pub struct ReviewCard {
    pub prompt: String,
    pub items: Vec<ReviewItem>,
    /// The diff when it was sent, e.g. "3 files · +5 −3".
    pub changes: String,
}

impl ReviewCard {
    pub fn ids(&self) -> Vec<u64> {
        self.items.iter().map(|i| i.id).collect()
    }
}

/// A `[c<id>] addressed: <note>` line, as `(id, note)`.
fn report(l: &str) -> Option<(u64, String)> {
    let l = l.trim().trim_start_matches(['-', '*', '`', ' ']);
    let (id, rest) = l.strip_prefix("[c")?.split_once(']')?;
    let rest = rest.trim_start();
    let note = rest
        .get(..10)
        .filter(|p| p.eq_ignore_ascii_case("addressed:"))
        .map(|_| &rest[10..])?;
    Some((
        id.parse().ok()?,
        note.trim().trim_end_matches('`').trim().to_string(),
    ))
}

/// The reply's lines (with their ends), each with its report when it is one; lines in a code
/// block are none.
fn report_lines(reply: &str) -> impl Iterator<Item = (&str, Option<(u64, String)>)> {
    let mut fence: Option<&str> = None;
    reply.split_inclusive('\n').map(move |l| {
        let ticks = l.trim_start();
        let ticks = &ticks[..ticks.len() - ticks.trim_start_matches('`').len()];
        let inside = fence.is_some();
        match fence {
            None if ticks.len() >= 3 => fence = Some(ticks),
            Some(f) if ticks.starts_with(f) && l.trim().len() == ticks.len() => fence = None,
            _ => {}
        }
        (l, (!inside && fence.is_none()).then(|| report(l)).flatten())
    })
}

/// The reply's `[c<id>] addressed: <note>` lines, as `(id, note)`.
pub fn parse_reports(reply: &str) -> Vec<(u64, String)> {
    report_lines(reply).filter_map(|(_, r)| r).collect()
}

/// The reply as shown: without its `[c<id>] addressed:` lines, which only feed the diff's
/// threads (src/app/review.rs).
fn hide_reports(reply: &str) -> String {
    (report_lines(reply).filter(|(_, r)| r.is_none()))
        .map(|(l, _)| l)
        .collect()
}

/// A reply that is only report lines: nothing to show. Stops at the first other line, so
/// it is cheap per frame.
fn hidden_reply(src: &str) -> bool {
    let mut any = false;
    for (l, r) in report_lines(src) {
        match r {
            Some(_) => any = true,
            None if l.trim().is_empty() => {}
            None => return false,
        }
    }
    any
}

/// Some line of `text` starts like a report (`[c<digit>`), unlike `#[cfg(...)]`.
fn may_report(text: &str) -> bool {
    text.lines().any(|l| {
        let l = l.trim_start().trim_start_matches(['-', '*', '`', ' ']);
        l.strip_prefix("[c")
            .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()))
    })
}

/// A review's prompt (review.rs `prompt`) back as its card, for a replayed transcript: each
/// `[c<id>] <place>` and its quoted text. The diff it was sent with is unknown by then.
fn card_of(prompt: &str) -> Option<ReviewCard> {
    // A workspace note may come before it on the wire.
    let prompt = &prompt[prompt.find("Review comments on the diff.")?..];
    let (mut items, mut fence) = (Vec::<ReviewItem>::new(), None);
    for l in prompt.lines() {
        if let Some(f) = fence {
            fence = (l != f).then_some(f);
            continue;
        }
        if l.starts_with("```") {
            fence = Some(l);
        } else if let Some((id, rest)) = l.strip_prefix("[c").and_then(|l| l.split_once("] "))
            && let Ok(id) = id.parse()
        {
            let place = rest.split(" · ").next().unwrap_or(rest).to_string();
            items.push(ReviewItem {
                id,
                place,
                text: String::new(),
            });
        } else if let (Some(t), Some(it)) = (l.strip_prefix("> "), items.last_mut()) {
            if !it.text.is_empty() {
                it.text.push('\n');
            }
            it.text.push_str(t);
        }
    }
    (!items.is_empty()).then(|| ReviewCard {
        prompt: prompt.to_string(),
        items,
        changes: String::new(),
    })
}

/// One comment of a review card: its id, "app.rs · Working copy · lines 631–632", its text.
pub struct ReviewItem {
    pub id: u64,
    pub place: String,
    pub text: String,
}

/// A transcript row: a block, or one call inside an expanded tool group.
type Row = (usize, Option<usize>);

/// A copy button whose icon turns into a check for 2s. Until `group` is hovered it is
/// invisible but laid out, so showing it never moves anything.
pub(crate) fn copy_button(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    group: &'static str,
) -> Div {
    div()
        .flex_none()
        .invisible()
        .group_hover(group, |s| s.visible())
        .child(Clipboard::new(id).value(text).tooltip("Copy"))
}

/// Markdown whose code blocks get the language and a copy button in their top-right corner.
/// The blocks keep the top padding the language label had, so the corner never covers code.
pub(crate) fn markdown(md: &Entity<TextViewState>, group: &'static str) -> TextView {
    TextView::new(md)
        .selectable(true)
        .style(TextViewStyle::default().code_block(StyleRefinement::default().pt(px(28.))))
        .code_block_actions(move |block, _, cx| {
            h_flex()
                .h(px(20.))
                .gap_1()
                .pl_1()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .when_some(block.lang().filter(|l| !l.is_empty()), |d, l| d.child(l))
                .child(copy_button("copy", block.code(), group))
        })
}

fn standalone(name: &str) -> Option<IconName> {
    match name {
        "Skill" => Some(IconName::BookOpen),
        "Agent" | "Task" => Some(IconName::Bot),
        // A submitted plan: its row opens the version in the plan pane.
        "ExitPlanMode" => Some(IconName::ClipboardList),
        // The agent's questions: the row shows the answers.
        "AskUserQuestion" => Some(IconName::MessageCircleQuestionMark),
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
    /// A review waiting for the turn to end; it goes out as its card.
    review: Option<Rc<ReviewCard>>,
    /// GitHub reviews are acknowledged only once this prompt is sent.
    git_reviews: Vec<String>,
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
            review: None,
            git_reviews: vec![],
        }
    }

    fn text(&self) -> String {
        self.content.text().trim().to_string()
    }
}

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
    /// The question card had the focus at the last render: when it goes, the composer gets it.
    asked_focused: bool,
    /// Plan versions and the pane that shows them, next to the transcript while `plan_open`.
    pub plan: Entity<PlanPane>,
    pub plan_open: bool,
    /// Files from Finder being dragged over the pane.
    dropping: Option<Vec<PathBuf>>,
    _subs: Vec<Subscription>,
}

/// Replies to tools that need a person, which claude asks us about (`Ev::Permission`).
const PLAN_SHOWN: &str = "The user now sees this plan in the app's plan panel, where they \
    approve it or reply with changes. End your turn with one short sentence, without repeating \
    the plan. If they ask for changes, update the plan file and call ExitPlanMode again.";
const ENTER_PLAN: &str = "Only the user turns plan mode on in this app (shift-tab). Carry on, or \
    suggest it in your reply.";
const NEEDS_APPROVAL: &str = "This needs the user's approval, which this app can't ask for. Do \
    without it, or tell the user what to run.";
const APPROVED: &str = "I approve the plan. Plan mode is off: implement it now.";
const BUILD_FRESH: &str = "Implement this plan. It was made and approved in another chat, so you \
    have none of that chat's context: read the code it touches before you change it.";

/// How long an interrupt may take before the process is stopped instead.
const INTERRUPT_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Every chat names one: claude gets `--model`, `codex:<model id>` runs `codex app-server` with
/// that model (see `codex::is_codex`).
pub(crate) const MODELS: &[&str] = &[
    "fable",
    "opus",
    "sonnet",
    "haiku",
    "codex:gpt-6.1-sol",
    "codex:gpt-6-astra",
];

/// Thinking levels, lowest first: claude's `--effort`, codex's `effort` (both models take all six,
/// per `model/list`). Haiku takes `--effort` too.
const CLAUDE_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const CODEX_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max", "ultra"];
/// A chat's level until one is picked.
const EFFORT: &str = "high";

/// The thinking levels `model` takes, lowest first.
pub(crate) fn efforts(model: &str) -> &'static [&'static str] {
    match codex::is_codex(Some(model)) {
        true => CODEX_EFFORTS,
        false => CLAUDE_EFFORTS,
    }
}

/// The level a chat runs at: its own (or `high`). One the model lacks is its highest if it was
/// `ultra`, else `high`, so an unknown value never picks the costliest.
pub(crate) fn effort_of(meta: &ChatMeta) -> &'static str {
    let levels = efforts(meta.model.as_deref().unwrap_or(MODELS[0]));
    let want = meta.effort.as_deref().unwrap_or(EFFORT);
    match levels.iter().find(|l| **l == want) {
        Some(l) => l,
        None if want == "ultra" => levels.last().unwrap(),
        None => levels
            .iter()
            .find(|l| **l == EFFORT)
            .unwrap_or(levels.last().unwrap()),
    }
}

/// A thinking level as people read it: "xhigh" → "Extra high".
pub(crate) fn effort_label(level: &str) -> String {
    match level {
        "xhigh" => "Extra high".into(),
        l => model_label(l),
    }
}

/// Shared menu contents: production actions and preview fixtures use the same model/effort rules.
pub(crate) fn model_entries(
    meta: &ChatMeta,
    choose_model: impl Fn(&'static str, &mut App) + Clone + 'static,
    choose_effort: impl Fn(&'static str, &mut App) + Clone + 'static,
) -> (Vec<crate::menu::Entry>, usize) {
    let current = meta.model.as_deref().unwrap_or(MODELS[0]);
    // Claude's models, then Codex's: switching between the two starts a new conversation.
    let mut entries = vec![];
    for (i, &m) in MODELS.iter().enumerate() {
        let is_codex = codex::is_codex(Some(m));
        if is_codex && !entries.iter().any(|e| matches!(e, crate::menu::Entry::Sep)) {
            entries.push(crate::menu::Entry::Sep);
        }
        let choose = choose_model.clone();
        let item = crate::menu::item(None, model_label(m), move |_, cx| {
            choose(m, cx);
        });
        // The backend where the name doesn't say it, and the ctrl-cmd-N pick.
        let keys = format!("⌃⌘{}", i + 1);
        let hint = if is_codex {
            format!("Codex  {keys}")
        } else {
            keys
        };
        entries.push(item.hint(hint).checked(m == current));
    }
    entries.push(crate::menu::Entry::Sep);
    let effort = effort_of(meta);
    let mut sel = 0;
    for &l in efforts(current) {
        if l == effort {
            sel = entries.len();
        }
        let choose = choose_effort.clone();
        let item = crate::menu::item(None, effort_label(l), move |_, cx| {
            choose(l, cx);
        });
        let hint = if matches!(l, "max" | "ultra") {
            "slower"
        } else {
            ""
        };
        entries.push(item.hint(hint).checked(l == effort));
    }
    (entries, sel)
}

/// A model id as people read it: "opus" → "Opus", "codex:gpt-6.1-sol" → "GPT-6.1 Sol".
pub(crate) fn model_label(id: &str) -> String {
    let cap = |w: &str| {
        let mut w = w.to_string();
        if let Some(c) = w.get_mut(..1) {
            c.make_ascii_uppercase();
        }
        w
    };
    match id.strip_prefix("codex:") {
        Some(m) => {
            let mut words = m.split('-');
            let head = match (words.next(), words.next()) {
                (Some("gpt"), Some(v)) => format!("GPT-{v}"),
                (a, b) => [a, b]
                    .into_iter()
                    .flatten()
                    .map(cap)
                    .collect::<Vec<_>>()
                    .join(" "),
            };
            std::iter::once(head)
                .chain(words.map(cap))
                .collect::<Vec<_>>()
                .join(" ")
        }
        None => cap(id),
    }
}

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

/// A chat started by a review: "Review · 3 comments".
fn review_title(r: &ReviewCard) -> String {
    let n = r.items.len();
    format!("Review · {n} comment{}", plural(n))
}

/// The "s" of `n` things.
pub(crate) fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
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
pub(crate) fn hint(keys: &str, label: &'static str) -> Div {
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

    pub fn model(&self) -> &str {
        self.meta.model.as_deref().unwrap_or(MODELS[0])
    }

    pub(crate) fn codex(&self) -> bool {
        codex::is_codex(self.meta.model.as_deref())
    }

    /// The tab label.
    pub fn title(&self) -> SharedString {
        match self.meta.title.is_empty() {
            true => "New chat".into(),
            false => self.meta.title.clone().into(),
        }
    }

    /// The thinking level this chat runs at (see `effort_of`).
    pub fn effort(&self) -> &'static str {
        effort_of(&self.meta)
    }

    /// The levels this chat's model takes, lowest first.
    pub fn efforts(&self) -> &'static [&'static str] {
        efforts(self.model())
    }

    /// The model button's label: "Opus · High".
    pub(crate) fn model_chip(&self) -> String {
        format!(
            "{} · {}",
            model_label(self.model()),
            effort_label(self.effort())
        )
    }

    /// cmd-/ or the model button: the models, then the thinking levels, the current ones
    /// checked; type a letter to jump (again to cycle). The highlight starts on the current
    /// level, the pick changed most. A pick takes effect on the next send.
    pub fn open_model_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let model_chat = cx.entity().downgrade();
        let effort_chat = model_chat.clone();
        let (entries, sel) = model_entries(
            &self.meta,
            move |m, cx| {
                model_chat.update(cx, |c, cx| c.set_model(m, cx)).ok();
            },
            move |l, cx| {
                effort_chat.update(cx, |c, cx| c.set_effort(l, cx)).ok();
            },
        );
        let b = self.model_at.get();
        let at = point(b.left(), b.top() - px(4.));
        let m = crate::menu::open_at(entries, at, Anchor::BottomLeft, window, cx);
        m.update(cx, |m, _| m.select(sel));
        let sub = cx.subscribe(&m, |c, _, _: &DismissEvent, cx| {
            c.model_menu = None;
            cx.notify();
        });
        self.model_menu = Some((m, sub));
        cx.notify();
    }

    /// The user's pick: the process restarts on it at the next send. A thinking level the new
    /// model lacks becomes its highest.
    pub fn set_model(&mut self, m: &str, cx: &mut Context<Self>) {
        let model = Some(m.to_string());
        if self.meta.model != model {
            let codex = codex::is_codex(model.as_deref());
            // A session belongs to one CLI: the other can't resume it.
            if codex != codex::is_codex(self.meta.model.as_deref())
                && self.meta.session_id.is_some()
            {
                self.meta.session_id = None;
                self.push(Block::Info(format!("{m} starts a new conversation")), cx);
            }
            self.meta.model = model;
            if self.meta.effort.is_some() {
                self.meta.effort = Some(self.effort().into());
            }
            cx.emit(ChatEvent::Meta);
            cx.notify();
        }
        cx.emit(ChatEvent::Picked);
    }

    /// The user's pick of thinking level (one the model lacks becomes its highest). Claude
    /// restarts on it at the next send, like a model change; codex sends it with the next
    /// message.
    pub fn set_effort(&mut self, level: &str, cx: &mut Context<Self>) {
        let was = self.meta.effort.replace(level.into());
        let level = self.effort();
        self.meta.effort = Some(level.into());
        if let Some(a) = &mut self.agent {
            a.set_effort(level);
        }
        if was != self.meta.effort {
            cx.emit(ChatEvent::Meta);
            cx.notify();
        }
        cx.emit(ChatEvent::Picked);
    }

    /// One thinking level up (`d` > 0) or down, stopping at the ends; the level it's on now.
    pub fn step_effort(&mut self, d: isize, cx: &mut Context<Self>) -> &'static str {
        let levels = self.efforts();
        let ix = levels.iter().position(|l| *l == self.effort()).unwrap_or(0);
        let ix = ix.saturating_add_signed(d).min(levels.len() - 1);
        self.set_effort(levels[ix], cx);
        levels[ix]
    }

    /// Plan mode on or off. A running process switches at once, mid-turn too; a new one
    /// switches as it starts.
    pub fn set_plan_mode(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.meta.plan == on {
            return;
        }
        self.meta.plan = on;
        if self.busy && self.codex() {
            self.push(
                Block::Info("codex switches plan mode from the next message".into()),
                cx,
            );
        }
        if let Some(a) = &mut self.agent
            && let Err(e) = a.set_plan_mode(on)
        {
            self.push(Block::Info(format!("switching plan mode failed: {e}")), cx);
        }
        // The pane says what will appear there; with nothing in it, it goes with the mode.
        if on || self.plan.read(cx).versions.is_empty() {
            self.plan_open = on;
        }
        cx.emit(ChatEvent::Meta);
        cx.notify();
    }

    /// codex's Fast service tier: its own default until toggled here.
    pub fn fast(&self) -> bool {
        self.meta.fast.unwrap_or(self.fast_by_config)
    }

    /// Fast on or off for a codex chat (callers check), from its next message (a running turn
    /// keeps its tier).
    pub fn set_fast(&mut self, on: bool, cx: &mut Context<Self>) {
        self.meta.fast = Some(on);
        if let Some(a) = &mut self.agent {
            a.set_fast(on);
        }
        cx.emit(ChatEvent::Meta);
        cx.notify();
    }

    /// Ends plan mode and tells the agent to build the latest plan. A draft rides along.
    pub fn approve_plan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let why = if self.plan.read(cx).latest().is_none() {
            "no plan to approve yet; the agent submits one in plan mode (shift-tab)"
        } else if self.busy {
            "the agent is still working; approve when the turn ends (ctrl-c stops it)"
        } else {
            let was = self.meta.plan;
            self.set_plan_mode(false, cx);
            if self.ask(APPROVED, window, cx) {
                self.plan_approved(cx);
                self.focus_composer(window, cx);
            } else {
                self.set_plan_mode(was, cx);
            }
            return;
        };
        self.push(Block::Info(why.into()), cx);
    }

    /// Marks the latest plan approved and closes the pane: there is nothing left to review.
    pub fn plan_approved(&mut self, cx: &mut Context<Self>) {
        self.plan.update(cx, |p, cx| p.approve(cx));
        self.plan_open = false;
        cx.notify();
    }

    /// The first message of a chat that builds another chat's plan: the plan and nothing else,
    /// titled after the plan.
    pub fn build_plan(&mut self, plan: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.meta.title = title_of(&plan::title(plan));
        self.ask(&format!("{BUILD_FRESH}\n\n{plan}"), window, cx)
    }

    /// `TogglePlan`: shows the plan pane and focuses it, or hides it.
    pub fn toggle_plan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.plan_open = !self.plan_open;
        match self.plan_open {
            true => self.plan.read(cx).focus.clone().focus(window, cx),
            false => self.focus_composer(window, cx),
        }
        cx.notify();
    }

    pub fn plan_focused(&self, window: &Window, cx: &App) -> bool {
        self.plan_open && self.plan.read(cx).focus.is_focused(window)
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
        let idle = self.vim.mode == vim::Mode::Normal && self.vim.pending().is_empty();
        // ctrl-c stops the agent here, and shift-tab toggles plan mode.
        let app_key = (ks.key == "c" && ks.modifiers.control)
            || ks.key == "tab"
            || (idle && matches!(ks.key.as_str(), "escape" | "enter"));
        let Some(key) = (!app_key).then(|| editor::vim_key(ks)).flatten() else {
            return false;
        };
        let Some((_, cmd)) = editor::apply_vim_key(&mut self.vim, &self.composer, &key, window, cx)
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

    /// Re-lists every repo's PRs in the background, at most once a minute: every `# heading` opens
    /// the menu. Each repo's list replaces its cached rows as it lands, so the menu shows progress
    /// per repo, and a repo that fails keeps its cached rows.
    fn refetch_prs(&mut self, cx: &mut Context<Self>) {
        let fresh = (self.prs_at).is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(60));
        if self.fetching_prs.is_some() || fresh || cfg!(test) {
            return;
        }
        let repos: Vec<_> = (self.ws.repos.iter())
            .map(|r| (r.name.clone(), r.src.clone()))
            .collect();
        let n = repos.len();
        (self.prs_left, self.prs_errs) = (n, vec![]);
        let (tx, rx) = std::sync::mpsc::channel();
        let bg = cx.background_executor().clone();
        bg.spawn(async move {
            crate::git::par_map(&repos, |(name, dir)| {
                let _ = tx.send((name.clone(), complete::fetch_repo_prs(name, dir, false)));
            });
        })
        .detach();
        self.fetching_prs = Some(cx.spawn(async move |this, cx| {
            let mut rx = rx;
            for _ in 0..n {
                let (got, back) = bg.spawn(async move { (rx.recv(), rx) }).await;
                rx = back;
                let Ok((repo, res)) = got else { break };
                this.update(cx, |c, cx| {
                    c.prs_left = c.prs_left.saturating_sub(1);
                    match res {
                        Ok(list) => {
                            c.prs.retain(|p| p.repo != repo);
                            c.prs.extend(list);
                            c.pr_items = complete::pr_items(&c.prs);
                        }
                        Err(e) => c.prs_errs.push((repo, e)),
                    }
                    c.update_menu(cx);
                })
                .ok();
            }
            this.update(cx, |c, cx| {
                c.fetching_prs = None;
                c.prs_left = 0;
                if c.prs_errs.is_empty() {
                    c.prs_at = Some(Instant::now());
                }
                // A repo that left the workspace takes its PRs with it.
                let repos = &c.ws.repos;
                c.prs.retain(|p| repos.iter().any(|r| r.name == p.repo));
                c.pr_items = complete::pr_items(&c.prs);
                c.update_menu(cx);
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
            let at = self.queue_slot();
            self.queue.insert(
                at,
                Queued {
                    git_reviews: std::mem::take(&mut self.editing_git_reviews),
                    ..Queued::new(content)
                },
            );
            self.qsel = at;
        } else if !sent {
            return; // spawn or write failed; the error is in the transcript, keep the draft
        }
        if sent {
            cx.emit(ChatEvent::GitReviewsSent(std::mem::take(
                &mut self.editing_git_reviews,
            )));
        }
        self.editing = None;
        self.composer
            .update(cx, |c, cx| c.set_value("", window, cx));
        self.reset_vim(cx);
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
        match self.send_block(q.text(), false, q.review.clone(), cx) {
            true => {
                self.qsel = self.qsel.saturating_sub(1);
                cx.emit(ChatEvent::GitReviewsSent(q.git_reviews));
            }
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
        if self.send_block(q.text(), true, q.review.clone(), cx) {
            // Claude may hold it unread until this turn ends; don't restart the process under it.
            self.steered |= busy;
            cx.emit(ChatEvent::GitReviewsSent(q.git_reviews));
        } else {
            self.editing = editing;
            self.queue.insert(self.qsel, q);
        }
        self.after_queue_change(cx);
        if self.queue.is_empty() {
            self.focus_composer(window, cx);
        }
    }

    /// Remove on a queued review card.
    pub(crate) fn remove_queued(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.qsel = ix;
        self.delete_queued(window, cx);
    }

    /// What went out, oldest first: messages, and reviews as their prompt.
    #[cfg(all(test, feature = "test-support"))]
    pub(crate) fn sent_texts(&self) -> Vec<String> {
        (self.blocks.iter())
            .filter_map(|b| match b {
                Block::User(t, _) => Some(t.clone()),
                Block::Review(r, _) => Some(r.prompt.clone()),
                _ => None,
            })
            .collect()
    }

    fn delete_queued(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.qsel >= self.queue.len() {
            return;
        }
        if let Some(r) = self.take_queued(self.qsel).review {
            cx.emit(ChatEvent::ReviewRemoved(r.ids()));
        }
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
        if self.queue.get(self.qsel).is_none_or(|q| q.review.is_some()) {
            return;
        }
        let mut ix = self.qsel;
        let item = self.take_queued(ix);
        let draft = self.composer.read(cx).content();
        if !draft.text().trim().is_empty() {
            let at = self.queue_slot();
            self.queue.insert(
                at,
                Queued {
                    git_reviews: std::mem::take(&mut self.editing_git_reviews),
                    ..Queued::new(draft)
                },
            );
            if at <= ix {
                ix += 1;
            }
        }
        self.editing = Some(ix);
        self.editing_git_reviews = item.git_reviews;
        let end = item.content.text().len();
        self.composer.update(cx, |c, cx| {
            c.set_value(item.content, window, cx);
            c.set_selected_range(end..end, cx);
            c.focus(window, cx);
        });
        self.after_queue_change(cx);
    }

    /// Appends a git prompt without changing the draft or an edit already in progress.
    pub fn enqueue(&mut self, prompt: &str, reviews: Vec<String>, cx: &mut Context<Self>) -> bool {
        if self.archiving {
            return false;
        }
        self.queue.push(Queued {
            git_reviews: reviews,
            ..Queued::new(prompt.to_string().into())
        });
        self.qsel = self.queue.len() - 1;
        self.drain(cx);
        self.after_queue_change(cx);
        true
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
            self.editing = None;
            self.composer
                .update(cx, |c, cx| c.set_value("", window, cx));
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
            self.background.clear(); // its reader is replaced below, so its Exited never arrives
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
                    self.reader = Some(Self::read_events(rx, Self::apply, cx));
                }
                Err(e) => {
                    let codex = crate::codex::is_codex(self.meta.model.as_deref());
                    let prog = if codex { "codex" } else { "claude" };
                    let why = crate::git::spawn_err(&self.ws.dir(), prog, e);
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

    /// Sends exactly a review's prompt, leaving the composer's draft alone. While a turn or
    /// git step runs it waits at the top of the queue. `ReviewSent` says when it goes out;
    /// false when it was refused (the transcript says why).
    pub fn send_review(&mut self, mut card: ReviewCard, cx: &mut Context<Self>) -> bool {
        // As the queue sends it, so claude's echo matches the card.
        card.prompt = card.prompt.trim().to_string();
        let card = Rc::new(card);
        if self.busy || self.hold {
            let q = Queued {
                review: Some(card.clone()),
                ..Queued::new(card.prompt.clone().into())
            };
            self.queue.insert(0, q);
            // The rows below it moved down one: the cursor and the edited slot follow them.
            self.qsel += 1;
            if let Some(e) = &mut self.editing {
                *e += 1;
            }
            self.after_queue_change(cx);
            return true;
        }
        self.send_block(card.prompt.clone(), false, Some(card), cx)
    }

    /// The ended turn's reply and number, for review reports: the text after the last message
    /// claude read (ones queued mid-turn and still unread don't start it).
    fn keep_reply(&mut self) {
        let start = (self.blocks.iter())
            .rposition(|b| matches!(b, Block::User(_, false) | Block::Review(_, false)));
        let after = start.map_or(0, |i| i + 1);
        self.last_reply = (self.blocks[after..].iter())
            .filter_map(|b| match b {
                Block::Text { src, .. } => Some(src.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        self.last_turn = (self.blocks[..after].iter())
            .filter(|b| matches!(b, Block::User(..) | Block::Review(..)))
            .count();
    }

    /// Messages in the transcript; the nth one starts turn n.
    pub fn turns(&self) -> usize {
        (self.blocks.iter())
            .filter(|b| matches!(b, Block::User(..) | Block::Review(..)))
            .count()
    }

    /// Comments of sent reviews claude hasn't echoed yet; with `held`, only ones sent mid-turn,
    /// which the turn that is ending never read.
    pub fn unread_review_ids(&self, held: bool) -> Vec<u64> {
        (self.unread.iter())
            .filter_map(|&bi| match self.blocks.get(bi) {
                Some(Block::Review(r, queued)) if *queued || !held => Some(r.ids()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    /// Comments of reviews still waiting to be read: the tab shows "Chat · N".
    pub fn review_count(&self) -> usize {
        let queued = self.queue.iter().filter_map(|q| q.review.as_ref());
        let unread = (self.unread.iter()).filter_map(|&bi| match self.blocks.get(bi) {
            Some(Block::Review(r, _)) => Some(r),
            _ => None,
        });
        queued.chain(unread).map(|r| r.items.len()).sum()
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
            let s = if n == 1 { "" } else { "s" };
            self.blocks.push(Block::Info(format!(
                "{n} message{s} not delivered; send again"
            )));
        }
    }

    /// The process is gone (or going): forget its state, end its calls, flag what it never read.
    fn drop_process(&mut self, cx: &mut Context<Self>) {
        self.agent = None; // drop stops it
        self.drop_asked(cx);
        self.background.clear();
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

    /// A submitted plan as the pane's newest version, shown.
    fn show_plan(&mut self, id: String, text: String, cx: &mut Context<Self>) {
        if !text.trim().is_empty() {
            self.plan.update(cx, |p, cx| p.add(id, text, cx));
            self.plan_open = true;
        }
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
        let bi = (self.blocks.iter())
            .rposition(|b| !matches!(b, Block::User(_, true) | Block::Review(_, true)))?;
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
                // A process of the backend the chat just switched away from: its session
                // can't be resumed by the new one.
                let stale = !self.replaying
                    && codex::is_codex(self.agent_model.as_deref())
                        != codex::is_codex(self.meta.model.as_deref());
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
            Ev::Background(tasks) => self.background = tasks,
            Ev::Interrupted => self.cut = true,
            // claude's echo: the oldest message it hadn't read is now part of the turn.
            // `ends_with`: a workspace `note` may have been put before the text on the wire.
            // Not just the oldest: one the CLI refused (a failed codex turn/start) is never echoed.
            Ev::User(text) => {
                let read = (self.unread.iter()).position(|&bi| match &self.blocks[bi] {
                    Block::User(t, _) => text.ends_with(t.as_str()),
                    Block::Review(r, _) => text.ends_with(r.prompt.as_str()),
                    _ => false,
                });
                match read {
                    Some(k) => {
                        let bi = self.unread.remove(k).unwrap();
                        if let Block::User(_, queued) | Block::Review(_, queued) =
                            &mut self.blocks[bi]
                        {
                            *queued = false;
                        }
                        self.remeasure(bi);
                        // codex reads a steer into the running turn, with no new init after.
                        if self.unread.is_empty() {
                            self.steered = false;
                        }
                    }
                    None => self.blocks.push(match card_of(&text) {
                        Some(card) => Block::Review(Rc::new(card), false),
                        None => Block::User(text, false),
                    }),
                }
            }
            Ev::TextDelta(s) => match self.live_text() {
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
            },
            Ev::Text(full) => match self.live_text() {
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
            },
            Ev::ToolUse { id, name, input } => {
                let call = Call {
                    summary: match name.as_str() {
                        "ExitPlanMode" => plan::title(input["plan"].as_str().unwrap_or_default()),
                        "AskUserQuestion" => question::asked_summary(&input),
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
            Ev::Permission {
                request_id,
                tool,
                tool_use_id,
                input,
            } => {
                // Questions wait for the user's answers in a card; the turn holds meanwhile.
                // (Replayed history has none: a request is only ever live.)
                if tool == "AskUserQuestion" && !self.replaying {
                    return self.show_asked(request_id, tool_use_id, input, cx);
                }
                // Plan mode's approval happens in the plan pane, after the turn: the agent is
                // told the plan was shown, and the turn ends instead of waiting on a person.
                let reason = match tool.as_str() {
                    "ExitPlanMode" => {
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
            } => {
                self.busy = false;
                if !self.replaying {
                    self.stopping = None; // a live interrupt's fallback outlives history
                    self.drop_asked(cx);
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
                    self.keep_reply();
                    cx.emit(ChatEvent::TurnDone);
                }
            }
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

    /// The list's last item, under the newest message: a quiet line while a turn or background
    /// tasks run; hovering lists the details. Fixed height, so it comes and goes
    /// without remeasuring or moving the transcript.
    fn render_activity(&self, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let line = div().id("activity").w_full().h(px(24.)).px_4();
        if !self.working() {
            return line;
        }
        let text = match (self.busy, self.background.len()) {
            _ if self.asking() => "Waiting for your answers".into(),
            (true, 0) => "Working".into(),
            (true, n) => format!("Working · {n} in background"),
            (false, n) => format!("{n} in background"),
        };
        let (busy, started, model) = (
            self.busy && !self.asking(),
            self.started,
            self.model().to_string(),
        );
        let (queued, steered, tasks) = (self.queued(), self.steered, self.background.clone());
        line.child(
            div().w_full().max_w(px(680.)).mx_auto().child(
                HoverCard::new("activity-card")
                    .anchor(Anchor::BottomLeft)
                    .trigger(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            // The shimmer redraws the chat every frame, as a tool's spinner
                            // does; only during a turn, not for tasks left running after it.
                            .map(|d| match busy {
                                true => d.child(ShimmerText::new(text).id("activity-text")),
                                false => d.child(text),
                            }),
                    )
                    .content(move |_, _, cx| {
                        let theme = cx.theme();
                        let line = |t: String| div().child(t);
                        v_flex()
                            .gap_1()
                            .text_xs()
                            .max_w(px(420.))
                            .when_some(started.filter(|_| busy), |d, t| {
                                d.child(line(format!(
                                    "Turn running for {}",
                                    duration(t.elapsed().as_secs())
                                )))
                            })
                            .child(line(format!("Model {model}")))
                            .when(queued > 0, |d| d.child(line(format!("{queued} queued"))))
                            .when(steered, |d| {
                                d.child(line("Message sent mid-turn, not read yet".into()))
                            })
                            .when(!tasks.is_empty(), |d| {
                                d.child(
                                    div()
                                        .pt_1()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("{} in background", tasks.len())),
                                )
                                .children(tasks.iter().map(|t| {
                                    h_flex()
                                        .gap_1p5()
                                        .child(Icon::new(IconName::Bot).with_size(px(12.)))
                                        .child(t.clone())
                                }))
                            })
                    }),
            ),
        )
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
            (Some(_), true) if !matches!(c.name.as_str(), "ExitPlanMode" | "AskUserQuestion") => {
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
                                    "ExitPlanMode" => "Plan".to_string(),
                                    "AskUserQuestion" => "Question".to_string(),
                                    _ => c.name.clone(),
                                }),
                        )
                        .child(
                            div().flex_1().truncate().child(
                                match (c.name.as_str(), plan.number_of(&c.id)) {
                                    // The call's own input can be stale: claude fills
                                    // in the plan file when it asks, not when it calls.
                                    ("ExitPlanMode", Some(n)) => {
                                        format!(
                                            "v{n} · {}",
                                            plan::title(&plan.versions[n - 1].text)
                                        )
                                    }
                                    ("AskUserQuestion", _) => (c.result.as_deref())
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
                    // Clipped, with a "truncated" note, when parsed (agent::MAX_OUTPUT).
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
                .child(markdown(md, "row")),
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

    /// A review as sent (screen 06): its comments, each linking to the diff, then a row that
    /// opens the diff. `footer` says where it stands.
    fn review_card(
        &self,
        r: &ReviewCard,
        key: usize,
        footer: Option<AnyElement>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let n = r.items.len();
        v_flex()
            .gap_2()
            .text_sm()
            .child(
                v_flex()
                    .gap_2()
                    .pl_3()
                    .py_2()
                    .border_l_2()
                    .border_color(theme.primary)
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.success_foreground)
                            .child(match n {
                                1 => "Diff comment added to context".to_string(),
                                n => format!("{n} diff comments added to context"),
                            }),
                    )
                    .children(r.items.iter().map(|it| {
                        let id = it.id;
                        v_flex()
                            .gap_0p5()
                            .child(
                                div()
                                    .id(("review-link", (key << 16) + id as usize))
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .cursor_pointer()
                                    .hover(|d| d.text_color(theme.foreground))
                                    .child(format!("{} ↗", it.place))
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.stop_propagation();
                                        cx.emit(ChatEvent::ShowComment(id))
                                    })),
                            )
                            .child(div().text_color(theme.foreground).child(it.text.clone()))
                    }))
                    .when_some(footer, |d, f| {
                        d.child(div().text_xs().text_color(theme.muted_foreground).child(f))
                    }),
            )
            .child(
                h_flex()
                    .id(("review-changes", key))
                    .h(px(32.))
                    .px_3()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .hover(|d| d.bg(theme.list_hover))
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.stop_propagation();
                        cx.emit(ChatEvent::ShowChanges)
                    }))
                    .child(div().flex_1().child("Review changes"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(r.changes.clone()),
                    ),
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
                if let Some(r) = &q.review {
                    let footer = h_flex()
                        .gap_1()
                        .child("Included with your next message ·")
                        .child(
                            div()
                                .id(("q-remove", ix))
                                .cursor_pointer()
                                .text_color(theme.foreground)
                                .hover(|d| d.underline())
                                .child("Remove")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.remove_queued(ix, window, cx);
                                    cx.stop_propagation();
                                })),
                        );
                    return div()
                        .px_2()
                        .py_1()
                        .rounded(theme.radius)
                        .when(sel, |d| d.bg(theme.list_active))
                        .child(self.review_card(r, ix, Some(footer.into_any_element()), cx))
                        .into_any_element();
                }
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
            (Block::User(t, _), _) => Some(t.clone()),
            (Block::Review(r, _), _) => Some(r.prompt.clone()),
            (Block::Text { src, .. }, _) => Some(src.clone()),
            (Block::Tool(c), _) if c.name == "ExitPlanMode" => {
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
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            window.push_notification("Copied", cx);
        }
    }

    fn toggle(&mut self, cx: &mut Context<Self>) {
        let Some(&(bi, call)) = self.rows.get(self.sel) else {
            return;
        };
        // A submitted plan opens in the plan pane rather than showing the tool's reply.
        if let Block::Tool(c) = &self.blocks[bi]
            && c.name == "ExitPlanMode"
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
        let theme = cx.theme();
        let (radius, border, card, muted) = (
            theme.radius_lg,
            theme.border,
            theme.secondary,
            theme.muted_foreground,
        );
        // The editor's Vim pill, on `muted`: the card is already `secondary`.
        let vim_pill = self.vim_on.then(|| {
            let p = self.vim.pending();
            div()
                .px_2()
                .h(px(20.))
                .flex()
                .items_center()
                .rounded_full()
                .font_family(theme.mono_font_family.clone())
                .bg(theme.muted)
                .child(match p.is_empty() {
                    true => self.vim.status().to_string(),
                    false => format!("{} {p}", self.vim.status()),
                })
        });
        let rows = list(
            self.list.clone(),
            cx.processor(|this, ix, window, cx| this.render_row(ix, window, cx)),
        )
        .size_full()
        .py_2();
        let drop_bg = theme.drop_target;
        let accent = theme.drag_border;
        let (bg, fg) = (theme.background.opacity(0.85), theme.foreground);
        let main = v_flex()
            .id("chat-pane")
            .group("chat-pane")
            .relative()
            .size_full()
            .on_action(cx.listener(|this, _: &FocusComposer, window, cx| {
                this.focus_composer(window, cx)
            }))
            // Files from Finder anywhere on the pane are copied in and mentioned in the composer.
            // Not `drag_over`/`on_drop`: gpui checks hover for those, and hover is off while the
            // last input was a key (a file drag doesn't count as the mouse), so a drag right
            // after typing showed nothing and dropped nothing.
            .on_drag_move(cx.listener(|this, e: &DragMoveEvent<ExternalPaths>, _, cx| {
                let over = (e.bounds.contains(&e.event.position)).then(|| e.drag(cx).paths().to_vec());
                if over.is_some() != this.dropping.is_some() {
                    cx.notify();
                }
                this.dropping = over;
            }))
            .on_file_drop_exit(cx.listener(|this, _, _, cx| {
                this.dropping = None;
                cx.notify();
            }))
            .child({
                let chat = cx.entity().downgrade();
                canvas(|_, _, _| {}, move |_, _, window, _| {
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
                })
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
                    .on_action(cx.listener(|this, _: &CopySelected, w, cx| this.copy_selected(w, cx)))
                    .on_action(cx.listener(|this, _: &FocusComposer, window, cx| {
                        this.focus_composer(window, cx)
                    }))
                    .child(rows)
                    .when(self.blocks.is_empty(), |d| {
                        d.child(
                            v_flex()
                                .absolute()
                                .inset_0()
                                .items_center()
                                .justify_center()
                                .gap_2()
                                .text_sm()
                                .text_color(crate::app::faint(theme))
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
                                ),
                        )
                    })
                    .vertical_scrollbar(&self.list),
            )
            .child(
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
                    ),
            )
            // No hitbox, so it never takes the drop or the mouse. A drag that ends elsewhere
            // leaves no active drag, so a missed exit can't leave it up.
            .when(self.dropping.is_some() && cx.has_active_drag(), |d| d.child(
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
                    ),
            ));
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
    fn model_menu_uses_supported_efforts_and_selects_the_effective_level() {
        use crate::menu::Entry;
        for (model, saved, selected, ultra) in [
            ("sonnet", "xhigh", "Extra high", false),
            ("sonnet", "ultra", "Max", false),
            ("codex:gpt-6.1-sol", "ultra", "Ultra", true),
        ] {
            let meta = ChatMeta {
                model: Some(model.into()),
                effort: Some(saved.into()),
                ..Default::default()
            };
            let (entries, sel) = model_entries(&meta, |_, _| {}, |_, _| {});
            assert!(
                matches!(&entries[sel], Entry::Item { label, checked: true, .. } if label == selected)
            );
            assert_eq!(
                entries
                    .iter()
                    .filter(|e| matches!(e, Entry::Item { checked: true, .. }))
                    .count(),
                2
            );
            assert!(
                entries
                    .iter()
                    .any(|e| matches!(e, Entry::Item { label, .. } if label == "Extra high"))
            );
            assert_eq!(
                entries
                    .iter()
                    .any(|e| matches!(e, Entry::Item { label, .. } if label == "Ultra")),
                ultra
            );
        }
    }

    /// A review prompt in a replayed transcript comes back as its card; report lines stay
    /// out of the shown reply.
    #[test]
    fn review_cards_and_reports() {
        let p = "note\n\nReview comments on the diff. Paths …:\n\n[c1] api/a.rs:12 · working tree\n````\n[c9] x\n> y\n````\n> rename\n> it\nSuggested change:\n```\nlet y;\n```\n\n[c4] api/b.rs:3-5 (old) · base 0123456789\n> keep\n\nEnd your reply with one line per comment you addressed, exactly like:\n[c<id>] addressed: <what you changed, in one line>\n";
        let card = card_of(p).unwrap();
        let items: Vec<_> = (card.items.iter())
            .map(|i| (i.id, i.place.as_str(), i.text.as_str()))
            .collect();
        assert_eq!(
            items,
            [
                (1, "api/a.rs:12", "rename\nit"),
                (4, "api/b.rs:3-5 (old)", "keep")
            ]
        );
        assert!(card.prompt.starts_with("Review comments"));
        assert!(card_of("plain [c1] x\n> y").is_none());
        assert_eq!(
            hide_reports("Done.\n[c1] addressed: x\n- `[c2] addressed: y`\nBye"),
            "Done.\nBye"
        );
        // One in a code block is an example, not a report.
        let fenced = "See:\n```\n[c1] addressed: example\n```\n[c2] addressed: real";
        assert_eq!(parse_reports(fenced), [(2, "real".to_string())]);
        assert_eq!(
            hide_reports(fenced),
            "See:\n```\n[c1] addressed: example\n```\n"
        );
        assert!(hidden_reply("[c1] addressed: x\n"));
        assert!(hidden_reply("\n- [c1] addressed: x\n[c2] addressed: y"));
        assert!(!hidden_reply("Done.\n[c1] addressed: x"));
        assert!(!hidden_reply("") && !hidden_reply("#[cfg(test)]"));
        assert!(may_report("x\n - `[c12] addr") && may_report("[c3"));
        assert!(!may_report("#[cfg(test)]\n[cx] y\n[c"));
    }

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
pub(crate) mod ui_tests {
    use super::*;

    /// Feeds recorded protocol lines to the chat as its process would.
    pub(crate) fn play(c: &mut Chat, lines: &[&str], cx: &mut Context<Chat>) {
        for l in lines {
            for ev in agent::parse(l) {
                c.apply(ev, cx);
            }
        }
    }
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin
    use gpui_kit::InputEvent as _;
    use gpui_kit::test::TestWindowExt as _;

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
        assert!(!agent::transcript_path(&meta.id).exists(), "no transcript");
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
            args.contains("--tools  --strict-mcp-config --safe-mode --no-session-persistence"),
            "{args}"
        );
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
                    let input = serde_json::json!({"plan": plan});
                    c.apply(
                        Ev::ToolUse {
                            id: id.into(),
                            name: "ExitPlanMode".into(),
                            input: input.clone(),
                        },
                        cx,
                    );
                    c.apply(
                        Ev::Permission {
                            request_id: "r".into(),
                            tool: "ExitPlanMode".into(),
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
        act(cx, &|w, cx| chat.update(cx, |c, cx| c.approve_plan(w, cx)));
        cx.update(|cx| {
            let c = chat.read(cx);
            assert!(!c.meta.plan, "approving ends plan mode");
            assert!(!c.plan_open, "and closes the plan pane");
            assert_eq!(c.plan.read(cx).approved, Some(1));
            assert!(matches!(c.blocks.last(), Some(Block::User(t, _)) if t == APPROVED));
        });
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
        cx.update(|cx| chat.update(cx, |c, _| c.agent = None));
        let _ = std::fs::remove_file(super::plan::tests_store_path(&ws.name));
        let _ = std::fs::remove_file(ws.dir());
    }

    /// The agent's questions: a card the keys answer, its reply to the agent, and esc or a
    /// message from the composer to answer in the chat instead.
    #[gpui_kit::test]
    fn answers_questions(cx: &mut TestAppContext) {
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
                            tool: "AskUserQuestion".into(),
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
                    Some(Block::Tool(t)) if t.name == "AskUserQuestion" => Some(t.summary.clone()),
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
                    c.blocks.iter().any(|b| matches!(b, Block::Tool(t) if t.name == "ExitPlanMode" && t.summary == "Cache")),
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

#[cfg(test)]
mod model_label_tests {
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin

    #[test]
    fn labels_read_like_names() {
        let l = super::model_label;
        assert_eq!(l("opus"), "Opus");
        assert_eq!(l("codex:gpt-6.1-sol"), "GPT-6.1 Sol");
        assert_eq!(l("codex:gpt-6-astra"), "GPT-6 Astra");
    }
}
