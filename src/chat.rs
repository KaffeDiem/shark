//! One agent conversation per workspace: transcript blocks, composer, and the claude process.
use crate::agent::{self, Agent, Ev};
use crate::state::Workspace;
use crate::*;
use gpui_kit::assets::IconName;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::{ActiveTheme, Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

enum Block {
    User(String),
    Text {
        md: Entity<TextViewState>,
        done: bool,
    },
    Tool {
        id: String,
        name: String,
        summary: String,
        result: Option<String>,
        is_error: bool,
        open: bool,
    },
    Info(String),
}

pub enum ChatEvent {
    SessionId(String),
    Model(Option<String>),
    TurnDone,
}

pub struct Chat {
    pub ws: Workspace,
    blocks: Vec<Block>,
    sel: usize,
    pub busy: bool,
    agent: Option<Agent>,
    pub(crate) composer: Entity<TextareaState>,
    /// Model the running process was started with; a mismatch respawns it on the next send.
    agent_model: Option<String>,
    scroll: ScrollHandle,
    pub focus: FocusHandle,
    reader: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

/// `default` means "no --model flag".
pub(crate) const MODELS: &[&str] = &["default", "fable", "opus", "sonnet", "haiku"];

impl EventEmitter<ChatEvent> for Chat {}

fn summarize(input: &Value) -> String {
    let field = [
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
    s.chars().take(120).collect()
}

impl Chat {
    pub fn new(ws: Workspace, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 10)
                .placeholder("Message the agent (Enter to send, Shift-Enter for newline, Esc for normal mode)")
                .submit_on_enter(true)
        });
        let _sub = cx.subscribe_in(&composer, window, |this, _, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { shift: false, .. } = ev {
                this.send(window, cx);
            }
        });
        let mut chat = Chat {
            ws,
            blocks: vec![],
            sel: 0,
            busy: false,
            agent: None,
            composer,
            agent_model: None,
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
            reader: None,
            _subs: vec![_sub],
        };
        // Replay history through the same path as live events.
        let history =
            std::fs::read_to_string(agent::transcript_path(&chat.ws.name)).unwrap_or_default();
        for ev in history.lines().flat_map(agent::parse) {
            chat.apply(ev, cx);
        }
        chat.busy = false;
        chat.sel = chat.blocks.len().saturating_sub(1);
        chat.scroll.scroll_to_bottom();
        chat
    }

    pub fn model(&self) -> &str {
        self.ws.model.as_deref().unwrap_or(MODELS[0])
    }

    /// Takes effect on the next send, which restarts the process on the new model.
    pub fn set_model(&mut self, m: &str, cx: &mut Context<Self>) {
        let model = (m != MODELS[0]).then(|| m.to_string());
        if self.ws.model != model {
            self.ws.model = model.clone();
            cx.emit(ChatEvent::Model(model));
            cx.notify();
        }
    }

    pub fn focus_composer(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |c, cx| c.focus(window, cx));
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().trim().to_string();
        if text.is_empty() || self.busy {
            return;
        }
        if self.agent.is_some() && self.agent_model != self.ws.model {
            self.agent = None; // drop stops it; the respawn resumes the same session on the new model
        }
        if self.agent.is_none() {
            match agent::spawn(&self.ws) {
                Ok((a, rx)) => {
                    self.agent = Some(a);
                    self.agent_model = self.ws.model.clone();
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
                    return;
                }
            }
        }
        if let Err(e) = self.agent.as_mut().unwrap().send(&text) {
            self.push(Block::Info(format!("send failed: {e}")), cx);
            return;
        }
        self.composer
            .update(cx, |c, cx| c.set_value("", window, cx));
        self.busy = true;
        // Jump to the end so push follows the new message and the reply.
        self.sel = self.blocks.len().saturating_sub(1);
        self.push(Block::User(text), cx);
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(a) = &self.agent {
            a.stop();
            self.push(Block::Info("stopping…".into()), cx);
        }
    }

    fn push(&mut self, b: Block, cx: &mut Context<Self>) {
        let follow = self.sel + 1 >= self.blocks.len();
        self.blocks.push(b);
        if follow {
            self.sel = self.blocks.len() - 1;
            self.scroll.scroll_to_bottom();
        }
        cx.notify();
    }

    fn apply(&mut self, ev: Ev, cx: &mut Context<Self>) {
        match ev {
            Ev::Init(id) => {
                if self.ws.session_id.as_ref() != Some(&id) {
                    self.ws.session_id = Some(id.clone());
                    cx.emit(ChatEvent::SessionId(id));
                }
            }
            Ev::User(text) => self.push(Block::User(text), cx),
            Ev::TextDelta(s) => match self.blocks.last_mut() {
                Some(Block::Text { md, done: false }) => {
                    md.update(cx, |m, cx| m.push_str(&s, cx));
                    self.scroll.scroll_to_bottom();
                }
                _ => {
                    let md = cx.new(|cx| TextViewState::markdown(&s, cx));
                    self.push(Block::Text { md, done: false }, cx);
                }
            },
            Ev::Text(full) => match self.blocks.last_mut() {
                Some(Block::Text { md, done }) if !*done => {
                    md.update(cx, |m, cx| m.set_text(&full, cx));
                    *done = true;
                }
                _ => {
                    let md = cx.new(|cx| TextViewState::markdown(&full, cx));
                    self.push(Block::Text { md, done: true }, cx);
                }
            },
            Ev::ToolUse { id, name, input } => {
                let summary = summarize(&input);
                self.push(
                    Block::Tool {
                        id,
                        name,
                        summary,
                        result: None,
                        is_error: false,
                        open: false,
                    },
                    cx,
                )
            }
            Ev::ToolResult {
                id,
                text,
                is_error: err,
            } => {
                for b in self.blocks.iter_mut().rev() {
                    if let Block::Tool {
                        id: tid,
                        result,
                        is_error,
                        ..
                    } = b
                        && *tid == id
                    {
                        *result = Some(text);
                        *is_error = err;
                        break;
                    }
                }
                cx.notify();
            }
            Ev::Result {
                is_error,
                text,
                cost,
            } => {
                self.busy = false;
                if is_error {
                    self.push(Block::Info(format!("error: {text}")), cx);
                } else {
                    self.push(Block::Info(format!("done · ${cost:.2}")), cx);
                }
                cx.emit(ChatEvent::TurnDone);
            }
            Ev::Exited(msg) => {
                self.agent = None;
                if self.busy {
                    self.busy = false;
                    self.push(Block::Info(msg), cx);
                    cx.emit(ChatEvent::TurnDone);
                }
                cx.notify();
            }
        }
    }

    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.sel = ix.min(self.blocks.len().saturating_sub(1));
        self.scroll.scroll_to_item(self.sel);
        cx.notify();
    }

    fn render_block(
        &self,
        ix: usize,
        b: &Block,
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
        let inner = match b {
            Block::User(t) => column()
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
            Block::Text { md, .. } => column()
                .text_size(px(14.))
                .line_height(relative(1.5))
                .child(TextView::new(md).selectable(true)),
            Block::Tool {
                name,
                summary,
                result,
                is_error,
                open,
                ..
            } => {
                let icon = |name: IconName| Icon::new(name).with_size(px(14.));
                let status = match (result, is_error) {
                    (None, _) => Spinner::new().with_size(px(14.)).into_any_element(),
                    (Some(_), true) => icon(IconName::X)
                        .text_color(theme.danger_foreground)
                        .into_any_element(),
                    _ => icon(IconName::Check).into_any_element(),
                };
                column()
                    .text_sm()
                    .child(
                        h_flex()
                            .h(px(28.))
                            .gap_2()
                            .text_color(theme.muted_foreground)
                            .child(icon(if *open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            }))
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.foreground)
                                    .child(name.clone()),
                            )
                            .child(div().flex_1().truncate().child(summary.clone()))
                            .child(status),
                    )
                    .when(*open, |d| {
                        let text: String = result
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
                                .text_color(if *is_error {
                                    theme.danger_foreground
                                } else {
                                    theme.foreground
                                })
                                .child(text),
                        )
                    })
            }
            Block::Info(t) => column()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(t.clone()),
        };
        row.child(inner).into_any_element()
    }

    fn toggle(&mut self, cx: &mut Context<Self>) {
        if let Some(Block::Tool { open, .. }) = self.blocks.get_mut(self.sel) {
            *open = !*open;
            cx.notify();
        }
    }
}

impl Render for Chat {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let theme = cx.theme();
        let (radius, border, card, muted) = (
            theme.radius_lg,
            theme.border,
            theme.secondary,
            theme.muted_foreground,
        );
        let blocks: Vec<AnyElement> = (0..self.blocks.len())
            .map(|ix| self.render_block(ix, &self.blocks[ix], focused, cx))
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
                div().px_4().pb_4().pt_1().child(
                    v_flex()
                        .key_context("Composer")
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
                                .child("send"),
                        ),
                ),
            )
    }
}
