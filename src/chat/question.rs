//! Questions the agent asks the user (claude's AskUserQuestion, codex's requestUserInput),
//! answered in a card above the composer while the turn waits. The card is gpui-component's
//! questionnaire: number keys pick, ↑/↓ move, enter goes on, the last question submits.
use super::*;
use gpui_kit::component::Size;
use gpui_kit::component::input::InputState;
use gpui_kit::component::questionnaire::*;
use serde_json::json;
use std::cell::Cell;

/// Told to the agent when the user closes the card (or sends a message instead).
pub(crate) const ANSWER_IN_CHAT: &str = "The user closed these questions to answer in the chat \
    instead. End your turn now without assuming any answers; their reply comes next.";

/// The agent's questions, waiting on the user. The turn holds until they're answered.
pub(super) struct Asked {
    request_id: String,
    tool_use_id: String,
    input: Value,
    qs: Vec<Q>,
    /// The asking process is codex's. Not `Chat::codex`: a model picked meanwhile only
    /// applies from the next message.
    codex: bool,
    /// Built on the first render, which has the window its inputs need.
    form: Option<Entity<QuestionnaireState>>,
    scroll: ScrollHandle,
    revealed: Cell<Option<(usize, usize, Pixels, Pixels)>>,
    _subs: Vec<Subscription>,
}

pub(super) struct Q {
    /// codex's; claude keys answers by the question's text.
    id: String,
    header: String,
    text: String,
    /// Label and description.
    options: Vec<(String, String)>,
    multi: bool,
    /// A free-text answer is allowed: always for claude, `isOther` for codex.
    other: bool,
    secret: bool,
}

/// What the user answered to one question: the labels picked and any text of their own.
type Answer = (Vec<String>, Option<String>);

/// The questions in a tool input, claude's or codex's.
pub(super) fn questions(input: &Value) -> Vec<Q> {
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
    (input["questions"].as_array().into_iter().flatten())
        .map(|q| {
            let options: Vec<(String, String)> = (q["options"].as_array().into_iter().flatten())
                .map(|o| (s(&o["label"]), s(&o["description"])))
                .collect();
            Q {
                id: s(&q["id"]),
                header: s(&q["header"]),
                text: s(&q["question"]),
                multi: q["multiSelect"].as_bool().unwrap_or(false),
                other: q["isOther"].as_bool().unwrap_or(true) || options.is_empty(),
                secret: q["isSecret"].as_bool().unwrap_or(false),
                options,
            }
        })
        .collect()
}

/// The questions answered, each with its picks and the user's own text joined with ", ".
fn given<'a>(
    qs: &'a [Q],
    answers: &'a [Answer],
) -> impl Iterator<Item = (&'a Q, &'a Answer, String)> {
    (qs.iter().zip(answers))
        .filter(|(_, a)| !a.0.is_empty() || a.1.is_some())
        .map(|(q, a)| {
            (
                q,
                a,
                a.0.iter()
                    .map(String::as_str)
                    .chain(a.1.as_deref())
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        })
}

/// What the agent gets back. claude: its input plus `answers` by question text. codex:
/// `answers` by question id, its own TUI's way (`None of the above` and a `user_note:` for
/// text of the user's own).
pub(super) fn reply(input: &Value, qs: &[Q], answers: &[Answer], codex: bool) -> Value {
    if codex {
        let by_id: serde_json::Map<String, Value> = given(qs, answers)
            .map(|(q, (labels, note), _)| {
                let mut list = labels.clone();
                if let Some(n) = note {
                    if list.is_empty() {
                        list.push("None of the above".into());
                    }
                    list.push(format!("user_note: {n}"));
                }
                (q.id.clone(), json!({ "answers": list }))
            })
            .collect();
        return json!({ "answers": by_id });
    }
    let by_text: serde_json::Map<String, Value> = given(qs, answers)
        .map(|(q, _, all)| (q.text.clone(), all.into()))
        .collect();
    let mut updated = input.clone();
    updated["answers"] = by_text.into();
    updated
}

/// `"Which DB?"="Postgres", …`: the answers as claude's tool result words them.
fn pairs(qs: &[Q], answers: &[Answer]) -> String {
    let pairs: Vec<String> = given(qs, answers)
        // Only the agent gets a secret; the row shows that one was given.
        .map(|(q, _, all)| {
            format!(
                "\"{}\"=\"{}\"",
                q.text,
                if q.secret { "••••" } else { &all }
            )
        })
        .collect();
    match pairs.is_empty() {
        true => "No answers".into(),
        false => pairs.join(", "),
    }
}

/// A question row's summary from claude's tool result, when it holds the answers.
pub(super) fn answered(result: &str) -> Option<&str> {
    // A clipped result has no reliable protocol suffix; keep all retained answer text.
    let truncated = result.rsplit_once("\n… truncated, ").is_some_and(|(_, n)| {
        n.strip_suffix(" chars total")
            .is_some_and(|n| n.parse::<u64>().is_ok())
    });
    if let Some(r) = result.strip_prefix("Your questions have been answered: ") {
        return Some(
            r.rsplit_once(". You can now continue")
                .filter(|_| !truncated)
                .map_or(r, |(answer, _)| answer),
        );
    }
    if let Some(r) = result.strip_prefix("The user answered: ") {
        return Some(
            r.rsplit_once(". Read the answers carefully")
                .filter(|_| !truncated)
                .map_or(r, |(answer, _)| answer),
        );
    }
    (result == ANSWER_IN_CHAT).then_some("Answered in the chat")
}

/// The questions as the row's summary until they're answered.
pub(super) fn asked_summary(input: &Value) -> String {
    let qs = questions(input);
    let texts: Vec<&str> = qs.iter().map(|q| q.text.as_str()).collect();
    texts.join(" · ")
}

impl Asked {
    pub(super) fn focused(&self, window: &Window, cx: &App) -> bool {
        (self.form.as_ref()).is_some_and(|f| f.read(cx).focus_handle().contains_focused(window, cx))
    }
}

impl Chat {
    /// The agent asked: show the card. A question still open is closed first, so its request
    /// gets an answer too.
    pub(super) fn show_asked(
        &mut self,
        request_id: String,
        tool_use_id: String,
        input: Value,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_asked(cx);
        let qs = questions(&input);
        let empty = qs.is_empty();
        self.asked = Some(Asked {
            request_id,
            tool_use_id,
            input,
            qs,
            codex: codex::is_codex(self.agent_model.as_deref()),
            form: None,
            scroll: ScrollHandle::new(),
            revealed: Cell::new(None),
            _subs: vec![],
        });
        // Nothing to show: closing it answers in each agent's own terms.
        match empty {
            true => self.dismiss_asked(cx),
            false => cx.emit(ChatEvent::Asking),
        }
    }

    pub(super) fn asked_form(&self) -> Option<Entity<QuestionnaireState>> {
        self.asked.as_ref()?.form.clone()
    }

    /// The agent waits on the user's answers.
    pub fn asking(&self) -> bool {
        self.asked.is_some()
    }

    /// The turn ended or the process went: what's still asked can't be answered anymore.
    pub(super) fn drop_asked(&mut self, cx: &mut Context<Self>) {
        if self.asked.take().is_some() {
            cx.emit(ChatEvent::Asking);
        }
    }

    /// Builds the form for questions just asked. Takes the focus when the user is in this
    /// chat with nothing typed, so they can answer right away.
    pub(super) fn build_asked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(asked) = &self.asked else { return };
        if asked.form.is_some() {
            return;
        }
        let mut others = vec![];
        let items: Vec<QuestionnaireItemDefinition> = (asked.qs.iter().enumerate())
            .map(|(i, q)| {
                // Indices, not labels: the form refuses two items or choices with one name.
                let choices = (q.options.iter().enumerate()).map(|(j, (label, desc))| {
                    let c = QuestionnaireChoiceDefinition::new(j.to_string(), label.clone());
                    match desc.is_empty() {
                        true => c,
                        false => c.with_description(desc.clone()),
                    }
                });
                let item = QuestionnaireItemDefinition::new(i.to_string(), q.text.clone())
                    .with_multiple(q.multi)
                    .with_choices(choices);
                match q.other {
                    true => {
                        let other = cx.new(|cx| {
                            InputState::new(window, cx)
                                .placeholder("Something else…")
                                .masked(q.secret)
                        });
                        others.push(other.clone());
                        item.with_input(QuestionnaireInputDefinition::new(other, "Other"))
                    }
                    false => item,
                }
            })
            .collect();
        // Item and choice names are indices, so the schema can't clash.
        let form = cx.new(|cx| {
            (QuestionnaireState::new(items, cx).expect("unique names"))
                .with_shortcuts(QuestionnaireShortcutMode::Numbers)
        });
        let mut subs = vec![cx.subscribe_in(
            &form,
            window,
            |this, _, ev: &QuestionnaireEvent, window, cx| match ev {
                QuestionnaireEvent::Submit(s) => this.answer_asked(s, window, cx),
                QuestionnaireEvent::CurrentItemChanged { .. } => {
                    if let Some(asked) = &this.asked {
                        asked.scroll.set_offset(Point::default());
                        asked.revealed.set(None);
                    }
                }
                _ => {}
            },
        )];
        // The form's own enter doesn't reach it from inside the field (the input takes it).
        for other in others {
            let f = form.clone();
            subs.push(cx.subscribe_in(
                &other,
                window,
                move |_, input, ev: &InputEvent, window, cx| {
                    if matches!(ev, InputEvent::PressEnter { .. })
                        && !input.read(cx).value().trim().is_empty()
                    {
                        f.update(cx, |f, cx| f.confirm_current(window, cx));
                    }
                },
            ));
        }
        let c = self.composer.read(cx);
        let here = self.focus.is_focused(window)
            || self.qfocus.is_focused(window)
            || (c.focus_handle(cx).is_focused(window) && c.value().trim().is_empty());
        if here {
            form.read(cx).focus_handle().clone().focus(window, cx);
        }
        if let Some(asked) = &mut self.asked {
            (asked.form, asked._subs) = (Some(form), subs);
        }
    }

    fn answer_asked(
        &mut self,
        sub: &QuestionnaireSubmission,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(asked) = self.asked.take() else {
            return;
        };
        let answers: Vec<Answer> = (0..asked.qs.len())
            .map(|i| {
                let Some(a) = sub.answer(&i.to_string()) else {
                    return (vec![], None);
                };
                let labels = (a.choices().iter())
                    .filter_map(|j| asked.qs[i].options.get(j.parse::<usize>().ok()?))
                    .map(|(l, _)| l.clone())
                    .collect();
                let note = a.freeform().map(|t| t.trim().to_string());
                (labels, note.filter(|t| !t.is_empty()))
            })
            .collect();
        let (reply, summary) = (
            reply(&asked.input, &asked.qs, &answers, asked.codex),
            pairs(&asked.qs, &answers),
        );
        let res = (self.agent.as_mut()).map_or(Ok(()), |a| a.allow(&asked.request_id, reply));
        self.answered(asked, summary, res, cx);
        self.focus_composer(window, cx);
    }

    /// esc on the card, or a message sent from the composer: the agent ends its turn and the
    /// user answers in the chat.
    pub(super) fn dismiss_asked(&mut self, cx: &mut Context<Self>) {
        let Some(asked) = self.asked.take() else {
            return;
        };
        let res = match (asked.codex, self.agent.as_mut()) {
            (_, None) => Ok(()),
            (true, Some(a)) => {
                let note = json!({"answers": [format!("user_note: {ANSWER_IN_CHAT}")]});
                let by_id: serde_json::Map<String, Value> = (asked.qs.iter())
                    .map(|q| (q.id.clone(), note.clone()))
                    .collect();
                a.allow(&asked.request_id, json!({ "answers": by_id }))
            }
            (false, Some(a)) => a.deny(&asked.request_id, ANSWER_IN_CHAT),
        };
        self.answered(asked, "Answered in the chat".into(), res, cx);
    }

    /// After a reply went out: codex's questions get a row of their own (claude's call has one).
    fn answered(
        &mut self,
        asked: Asked,
        summary: String,
        res: std::io::Result<()>,
        cx: &mut Context<Self>,
    ) {
        if let Err(e) = res {
            self.push(Block::Info(format!("answering the agent failed: {e}")), cx);
        } else if asked.codex {
            let id = asked.tool_use_id;
            let result = if summary == "Answered in the chat" {
                ANSWER_IN_CHAT.to_string()
            } else {
                format!("Your questions have been answered: {summary}. You can now continue")
            };
            let records = [
                json!({"type":"assistant", "shark_completed_answer":true, "message":{"content":[{"type":"tool_use", "id":id, "name":"AskUserQuestion", "input":asked.input}]}}),
                json!({"type":"user", "shark_completed_answer":true, "message":{"content":[{"type":"tool_result", "tool_use_id":id, "content":result, "is_error":false}]}}),
            ].map(|v| agent::compact(&v.to_string()).into_owned());
            if !self.completed_answers.contains_key(&id) {
                for record in &records {
                    for event in agent::parse(record) {
                        self.apply(event, cx);
                    }
                }
                self.completed_answers.insert(id, records);
            }
            self.retry_history(cx);
        }
        cx.emit(ChatEvent::Asking);
        cx.notify();
    }

    pub(crate) fn retry_history(&mut self, cx: &mut Context<Self>) {
        let path = agent::transcript_path(&self.meta.id);
        let mut failure = None;
        // shortcut: retries stay at EOF; add sequence metadata if replay must restore their original order.
        self.completed_answers.retain(|id, records| {
            match agent::append_answer(&path, id, records) {
                Ok(()) => false,
                Err(e) => {
                    failure = Some(e.to_string());
                    true
                }
            }
        });
        if failure.is_some() && self.history_save_error.is_none() {
            self.push(
                Block::Info("Answer sent, but could not save it to history".into()),
                cx,
            );
        }
        self.history_save_error = failure;
        cx.notify();
    }

    /// The question card, just above the composer like the side-question card.
    pub(super) fn render_asked(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let asked = self.asked.as_ref()?;
        let form = asked.form.as_ref()?;
        let (focused, at) = (
            asked.focused(window, cx),
            form.read(cx).current_ix().unwrap_or(0),
        );
        let last = at + 1 == asked.qs.len();
        let q = &asked.qs[at.min(asked.qs.len() - 1)];
        let max_height = window.viewport_size().height * 0.5;
        let answer = form
            .read(cx)
            .focused_current_choice(window)
            .and_then(|choice| choice.parse::<usize>().ok())
            .or_else(|| {
                form.read(cx)
                    .is_current_input_focused(window)
                    .then_some(q.options.len())
            })
            .map(|choice| (at, choice));
        let viewport = window.viewport_size();
        let target = answer.map(|(item, choice)| (item, choice, viewport.width, viewport.height));
        let reveal = asked.revealed.replace(target) != target;
        let items: Vec<_> = (asked.qs.iter().enumerate())
            .map(|(i, q)| {
                let name = i.to_string();
                let choices: Vec<_> = (0..q.options.len())
                    .map(|j| {
                        let anchor = ScrollAnchor::for_handle(asked.scroll.clone());
                        if reveal && answer == Some((i, j)) {
                            anchor.scroll_to(window, cx);
                        }
                        div()
                            .id(("asked-choice", j))
                            .test_support()
                            .anchor_scroll(Some(anchor))
                            .child(
                                QuestionnaireChoice::new(form, name.clone(), j.to_string())
                                    .content_style(StyleRefinement::default().min_w_0()),
                            )
                    })
                    .collect();
                let input_anchor = ScrollAnchor::for_handle(asked.scroll.clone());
                if reveal && answer == Some((i, q.options.len())) {
                    input_anchor.scroll_to(window, cx);
                }
                QuestionnaireItem::new(form, name.clone())
                    .child(QuestionnaireTitle::new(form, name.clone()))
                    .child(QuestionnaireChoices::new(form, name.clone()).children(choices))
                    .child(
                        div()
                            .id(("asked-input", i))
                            .anchor_scroll(Some(input_anchor))
                            .child(QuestionnaireInput::new(form, name.clone())),
                    )
                    .child(QuestionnaireError::new(form, name))
            })
            .collect();
        let theme = cx.theme();
        let header = h_flex()
            .flex_none()
            .gap_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(Icon::new(IconName::MessageCircleQuestionMark).with_size(px(14.)))
            .child(match q.header.is_empty() {
                true => "The agent asks".to_string(),
                false => format!("The agent asks · {}", q.header),
            })
            .child(div().flex_1())
            .when(asked.qs.len() > 1, |d| {
                d.child(QuestionnaireProgress::new(form).with_size(Size::XSmall))
            })
            .child(
                Button::new("dismiss-asked")
                    .ghost()
                    .xsmall()
                    .icon(IconName::X)
                    .tooltip("Answer in the chat instead (esc)")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.dismiss_asked(cx);
                        this.focus_composer(window, cx);
                    })),
            );
        let actions = QuestionnaireActions::new(form)
            .flex_none()
            .child(QuestionnairePrevious::new(form))
            .child(QuestionnaireSkip::new(form))
            .child(QuestionnaireNext::new(form))
            .child(QuestionnaireSubmit::new(form));
        let faint = crate::ui::faint(theme);
        let hints = h_flex()
            .flex_none()
            .flex_wrap()
            .gap_3()
            .text_xs()
            .text_color(faint)
            .child(hint("1", "pick"))
            .child(hint("up down", "move"))
            .when(q.multi, |d| d.child(hint("space", "tick")))
            .child(hint("enter", if last { "send" } else { "next" }))
            .child(hint("escape", "answer in chat"));
        Some(
            v_flex()
                .id("asked")
                .w_full()
                .max_w(px(712.))
                .max_h(max_height)
                .mx_auto()
                .mb_2()
                .p_3()
                .gap_2()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(if focused { theme.primary } else { theme.border })
                .bg(theme.muted)
                // A key reveals the focused answer again after the user wheels away from it.
                .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, w, cx| {
                    if ev.keystroke.modifiers.number_of_modifiers() == 0
                        && let Some(asked) = &this.asked
                        && asked
                            .form
                            .as_ref()
                            .is_some_and(|f| !f.read(cx).is_current_input_focused(w))
                    {
                        asked.revealed.set(None);
                        cx.notify();
                    }
                }))
                // esc in the "something else" field goes back to the choices; on them it closes.
                .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                    if let Some(f) = this.asked_form() {
                        f.read(cx).focus_handle().clone().focus(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .on_action(cx.listener(|this, _: &CancelEdit, window, cx| {
                    this.dismiss_asked(cx);
                    this.focus_composer(window, cx);
                }))
                .child(header)
                .child(
                    Questionnaire::new(form)
                        .with_size(Size::Small)
                        .min_h_0()
                        .child(
                            v_flex()
                                .id("asked-body")
                                .test_support()
                                .min_h_0()
                                .overflow_y_scroll()
                                .track_scroll(&asked.scroll)
                                .children(items),
                        )
                        .child(actions),
                )
                .map(|d| match focused {
                    true => d.child(hints),
                    false => d.child(
                        h_flex()
                            .flex_none()
                            .text_xs()
                            .text_color(faint)
                            .child(hint("up", "from the message box to answer")),
                    ),
                })
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin

    #[cfg(feature = "test-support")]
    #[gpui_kit::test]
    fn long_choices_wrap_inside_the_question_card(cx: &mut TestAppContext) {
        use gpui_kit::test::{TestQueryExt, TestWindowExt};

        crate::state::TEST_HOME
            .get_or_init(|| std::env::temp_dir().join(format!("shark-ui-{}", std::process::id())));
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cx.update(gpui_kit::init);
        for width in [420., 800.] {
            for multi in [false, true] {
                let (window, chat) = cx
                    .update(|cx| {
                        gpui_kit::open_window(
                            WindowOptions {
                                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                    point(px(0.), px(0.)),
                                    size(px(width), px(900.)),
                                ))),
                                ..Default::default()
                            },
                            cx,
                            |w, cx| {
                                cx.new(|cx| {
                                    Chat::new(
                                        Workspace::default(),
                                        ChatMeta::default(),
                                        false,
                                        w,
                                        cx,
                                    )
                                })
                            },
                        )
                    })
                    .unwrap();
                cx.update_window(window, |_, w, cx| {
                    chat.update(cx, |c, cx| {
                        c.show_asked("wrap".into(), "wrap".into(), json!({"questions": [{
                            "question": "What should the first release aim to deliver?",
                            "multiSelect": multi,
                            "options": [
                                {"label": "Core IDE editing (Recommended)", "description": "Definition and back/forward, completion, hover, diagnostics, references, rename, formatting, and quick fixes."},
                                {"label": "Navigation first", "description": "Short description."}
                            ]
                        }]}), cx);
                        cx.notify();
                    });
                    w.render_frame(cx);
                    let long = w.find_by_label("Core IDE editing (Recommended)").bounds();
                    let short = w.find_by_label("Navigation first").bounds();
                    assert!(long.left() >= px(0.) && long.right() <= px(width));
                    assert!(long.size.width <= px(712.));
                    assert!(long.size.height > short.size.height,
                        "long descriptions must wrap at width {width}, multiple={multi}: {long:?} vs {short:?}");
                }).unwrap();
            }
        }
    }

    #[cfg(feature = "test-support")]
    #[gpui_kit::test]
    fn review_wrapped_choices_keep_submit_inside_viewport(cx: &mut TestAppContext) {
        use gpui_kit::test::{TestEventExt, TestQueryExt, TestWindowExt};

        crate::state::TEST_HOME
            .get_or_init(|| std::env::temp_dir().join(format!("shark-ui-{}", std::process::id())));
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cx.update(gpui_kit::init);
        for (width, height) in [(420., 400.), (420., 600.), (800., 550.)] {
            for multi in [false, true] {
                let (window, chat) = cx
                    .update(|cx| {
                        gpui_kit::open_window(
                            WindowOptions {
                                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                    point(px(0.), px(0.)),
                                    size(px(width), px(height)),
                                ))),
                                ..Default::default()
                            },
                            cx,
                            |w, cx| {
                                cx.new(|cx| {
                                    Chat::new(
                                        Workspace::default(),
                                        ChatMeta::default(),
                                        false,
                                        w,
                                        cx,
                                    )
                                })
                            },
                        )
                    })
                    .unwrap();
                cx.update_window(window, |_, w, cx| {
                    chat.update(cx, |c, cx| {
                        c.show_asked("review-wrap".into(), "review-wrap".into(), json!({"questions": [{
                            "question": "What should the first release aim to deliver?",
                            "multiSelect": multi,
                            "options": [
                                {"label": "Core editing", "description": "Definition and back/forward, completion, hover, diagnostics, references, rename, formatting, and quick fixes."},
                                {"label": "Navigation", "description": "Workspace search, file search, symbol navigation, call hierarchy, breadcrumbs, pinned files, and recent workspace switching."},
                                {"label": "Review", "description": "Review comments, side by side diffs, inline editing, staging changes, resolving conflicts, and sharing feedback with the agent."},
                                {"label": "Collaboration", "description": "Shared workspaces, team settings, project templates, permissions, remote sessions, and notifications about important changes."}
                            ]
                        }]}), cx);
                        cx.notify();
                    });
                    w.render_frame(cx);
                    let submit = w.find_by_label("Submit").bounds();
                    assert!(submit.top() >= px(0.) && submit.bottom() <= px(height),
                        "Submit at {width}x{height}, multiple={multi}: {submit:?}");
                    let body = w.find("asked-body").bounds();
                    assert!(body.size.height > px(24.) && submit.top() >= body.bottom());
                    // A real wheel event can reach the fourth option without moving focus.
                    for _ in 0..30 {
                        let last = w.find_by_label("Collaboration").bounds();
                        if last.top() >= body.top() && last.top() + px(24.) <= body.bottom() { break; }
                        w.scroll("asked-body", ScrollDelta::Pixels(point(px(0.), px(-24.))), cx);
                    }
                    let last = w.find_by_label("Collaboration").bounds();
                    assert!(last.top() >= body.top() && last.top() + px(24.) <= body.bottom(),
                        "wheel must reveal last choice: {last:?} in {body:?}");
                    let form = chat.read(cx).asked_form().unwrap();
                    form.read(cx).focus_handle().clone().focus(w, cx);
                    w.key_down("down", false, cx);
                    w.key_up("down", cx);
                    assert_eq!(form.read(cx).focused_current_choice(w).map(|c| c.as_ref()), Some("0"), "arrow key focus");
                    w.render_frame(cx);
                }).unwrap();
                cx.run_until_parked();
                cx.update_window(window, |_, w, cx| {
                    w.simulate_next_frame(cx);
                    w.render_frame(cx);
                    let body = w.find("asked-body").bounds();
                    let first = w.find_by_label("Core editing").bounds();
                    assert!(
                        first.top() >= body.top() && first.top() + px(24.) <= body.bottom(),
                        "arrow key must reveal first choice: {first:?} in {body:?}"
                    );
                    for _ in 0..3 {
                        w.key_down("down", false, cx);
                        w.key_up("down", cx);
                    }
                    if multi {
                        w.press("4", cx);
                    }
                    w.render_frame(cx);
                })
                .unwrap();
                cx.run_until_parked();
                cx.update_window(window, |_, w, cx| {
                    w.simulate_next_frame(cx);
                    w.render_frame(cx);
                    let body = w.find("asked-body").bounds();
                    let last = w.find_by_label("Collaboration").bounds();
                    assert!(
                        last.top() >= body.top() && last.top() + px(24.) <= body.bottom(),
                        "arrow key must reveal last choice: {last:?} in {body:?}"
                    );
                    if multi {
                        w.scroll(
                            "asked-body",
                            ScrollDelta::Pixels(point(px(0.), px(2000.))),
                            cx,
                        );
                        let scrolled = w.find_by_label("Collaboration").bounds();
                        assert!(
                            scrolled.top() > body.bottom(),
                            "wheel must stay where the user scrolls"
                        );
                        // Toggle the same focused option off/on: focus identity does not change.
                        for _ in 0..2 {
                            w.press("4", cx);
                        }
                        w.simulate_next_frame(cx);
                        w.render_frame(cx);
                        let revealed = w.find_by_label("Collaboration").bounds();
                        assert!(
                            revealed.top() >= body.top()
                                && revealed.top() + px(24.) <= body.bottom()
                        );
                    }
                    let form = chat.read(cx).asked_form().unwrap();
                    if multi {
                        assert_eq!(
                            form.read(cx).answer("0").unwrap().choices(),
                            &[SharedString::from("3")],
                            "numeric multi-select at {width}x{height}"
                        );
                    }
                    w.resize(size(px(420.), px(400.)));
                    w.bounds_changed(cx);
                    w.render_frame(cx);
                })
                .unwrap();
                cx.run_until_parked();
                cx.update_window(window, |_, w, cx| {
                    w.simulate_next_frame(cx);
                    w.render_frame(cx);
                    let body = w.find("asked-body").bounds();
                    let last = w.find_by_label("Collaboration").bounds();
                    assert!(
                        last.top() >= body.top() && last.top() + px(24.) <= body.bottom(),
                        "resize must retain focused choice: {last:?} in {body:?}"
                    );
                    let form = chat.read(cx).asked_form().unwrap();
                    if !multi {
                        w.click_at(("asked-choice", 3usize), point(px(70.), px(12.)), cx);
                    }
                    assert_eq!(
                        form.read(cx).answer("0").unwrap().choices(),
                        &[SharedString::from("3")],
                        "selected response at {width}x{height}, multi={multi}"
                    );
                    let submit = w.find_by_label("Submit");
                    assert!(submit.bounds().bottom() <= px(400.));
                    w.click(
                        SharedString::from(format!("questionnaire-{}-Submit", form.entity_id())),
                        cx,
                    );
                })
                .unwrap();
                cx.run_until_parked();
                cx.update(|cx| {
                    assert!(
                        !chat.read(cx).asking(),
                        "visible Submit must complete the question"
                    )
                });
                cx.update_window(window, |_, w, _| w.remove_window())
                    .unwrap();
            }
        }
    }

    #[cfg(feature = "test-support")]
    #[gpui_kit::test]
    fn completed_answers_retry_and_replay_without_secrets(cx: &mut TestAppContext) {
        crate::state::TEST_HOME
            .get_or_init(|| std::env::temp_dir().join(format!("shark-ui-{}", std::process::id())));
        use std::fs;
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cx.update(gpui_kit::init);
        let id = format!("answer-retry-{}", std::process::id());
        let path = agent::transcript_path(&id);
        fs::create_dir_all(&path).unwrap(); // force a storage failure
        let (_, chat) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
                    cx.new(|cx| {
                        Chat::new(
                            Workspace::default(),
                            ChatMeta {
                                id: id.clone(),
                                ..Default::default()
                            },
                            false,
                            w,
                            cx,
                        )
                    })
                })
            })
            .unwrap();
        cx.run_until_parked();
        let input = json!({"questions":[{"id":"secret","question":"Token?","isSecret":true}]});
        let qs = questions(&input);
        let summary = pairs(&qs, &[(vec![], Some("NEVER_STORE_THIS_SECRET".into()))]);
        chat.update(cx, |c, cx| {
            let asked = Asked { request_id:"rpc".into(), tool_use_id:"question-1".into(), input, qs, codex:true, form:None, scroll:ScrollHandle::new(), revealed:Cell::new(None), _subs:vec![] };
            c.answered(asked, summary, Ok(()), cx);
            assert!(c.history_save_error.is_some());
            assert_eq!(c.completed_answers.len(), 1);
            assert!(c.blocks.iter().any(|b| matches!(b, Block::Info(s) if s == "Answer sent, but could not save it to history")));
        });
        fs::remove_dir(&path).unwrap();
        chat.update(cx, |c, cx| {
            c.retry_history(cx);
            c.retry_history(cx);
            assert!(c.history_save_error.is_none());
        });
        let stored = fs::read_to_string(&path).unwrap();
        assert!(!stored.contains("NEVER_STORE_THIS_SECRET"));
        assert_eq!(stored.lines().count(), 2);
        assert!(stored.lines().all(|line| {
            serde_json::from_str::<Value>(line).unwrap()["shark_completed_answer"] == true
        }));
        let history = agent::read_history(&path, fs::metadata(&path).unwrap().len());
        chat.update(cx, |c, cx| {
            let live = c
                .blocks
                .iter()
                .find_map(|b| match b {
                    Block::Tool(t) => Some((t.input.clone(), t.result.clone(), t.summary.clone())),
                    _ => None,
                })
                .unwrap();
            c.blocks.clear();
            for ev in history {
                c.apply(ev, cx);
            }
            let Block::Tool(replayed) = &c.blocks[0] else {
                panic!()
            };
            assert_eq!(
                (
                    replayed.input.clone(),
                    replayed.result.clone(),
                    replayed.summary.clone()
                ),
                live
            );
            let input = json!({"questions":[]});
            c.answered(
                Asked {
                    request_id: "rpc2".into(),
                    tool_use_id: "question-2".into(),
                    input,
                    qs: vec![],
                    codex: true,
                    form: None,
                    scroll: ScrollHandle::new(),
                    revealed: Cell::new(None),
                    _subs: vec![],
                },
                "Answered in the chat".into(),
                Ok(()),
                cx,
            );
        });
        assert!(fs::read_to_string(&path).unwrap().contains(ANSWER_IN_CHAT));
        fs::remove_file(path).unwrap();
    }

    /// Real Codex reply through the form, including a failed local save and exact replay.
    #[cfg(feature = "test-support")]
    #[gpui_kit::test]
    #[ignore]
    fn live_completed_answer_retry(cx: &mut TestAppContext) {
        use std::{
            fs,
            time::{Duration, Instant},
        };
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::remove_var("SHARK_CODEX");
        }
        crate::state::TEST_HOME.get_or_init(|| {
            std::env::temp_dir().join(format!("shark-live-answer-{}", std::process::id()))
        });
        cx.update(gpui_kit::init);
        let ws = Workspace {
            name: format!("live-answer-{}", std::process::id()),
            ..Default::default()
        };
        fs::create_dir_all(ws.dir()).unwrap();
        let meta = ChatMeta {
            id: ws.name.clone(),
            model: Some("codex:gpt-6.1-sol".into()),
            effort: Some("low".into()),
            plan: true,
            ..Default::default()
        };
        let path = agent::transcript_path(&meta.id);
        fs::create_dir_all(&path).unwrap();
        let (window, chat) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
                    cx.new(|cx| Chat::new(ws.clone(), meta.clone(), false, w, cx))
                })
            })
            .unwrap();
        cx.run_until_parked();
        // Separate wire records so the injected local-answer failure cannot lose live events.
        let wire = ChatMeta {
            id: format!("{}-wire", meta.id),
            ..meta.clone()
        };
        let (mut process, rx) = agent::spawn(&ws, &wire).unwrap();
        process.send("Use request_user_input exactly once to ask for a public synthetic test marker, offering options Red and Blue. The user will give their own text instead. Once answered, reply only with the final four digits of that marker. Never repeat the full marker or write it anywhere.").unwrap();
        chat.update(cx, |c, _| {
            c.agent = Some(process);
            c.agent_model = meta.model.clone();
        });
        const SENTINEL: &str = "SHARK_SYNTHETIC_7391";
        let end = Instant::now() + Duration::from_secs(180);
        let (mut requests, mut completed, mut response) = (0, false, String::new());
        while Instant::now() < end {
            let ev = match rx.try_recv() {
                Ok(ev) => ev,
                Err(e) if e.is_closed() => panic!("live Codex channel closed before completion"),
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
            };
            match &ev {
                Ev::Text(text) | Ev::Plan { text, .. } => response.push_str(text),
                Ev::Result { is_error, text, .. } => {
                    assert!(!is_error, "{text}");
                    completed = true;
                }
                Ev::Exited(error) => panic!("live Codex exited: {error}"),
                _ => {}
            }
            chat.update(cx, |c, cx| c.apply(ev, cx));
            if cx.update(|cx| chat.read(cx).asking()) {
                requests += 1;
                assert_eq!(requests, 1, "the service must ask exactly once");
                cx.update_window(window, |_, w, cx| {
                    chat.update(cx, |c, cx| {
                        let asked = c.asked.as_ref().unwrap();
                        assert_eq!(asked.qs.len(), 1);
                        assert!(!asked.qs[0].secret, "this live question is public");
                        c.build_asked(w, cx);
                        c.asked_form().unwrap().update(cx, |form, cx| {
                            form.set_answer(
                                "0",
                                QuestionnaireAnswer::new().with_freeform(SENTINEL),
                                w,
                                cx,
                            )
                            .unwrap();
                            assert!(form.submit(w, cx));
                        });
                    });
                })
                .unwrap();
                cx.run_until_parked();
                chat.update(cx, |c, _| {
                    assert!(
                        !c.asking(),
                        "production form submission answered the service"
                    );
                    assert!(c.history_save_error.is_some());
                    assert_eq!(c.completed_answers.len(), 1);
                });
            }
            if completed {
                break;
            }
        }
        chat.update(cx, |c, _| c.agent = None);
        let stop_by = Instant::now() + Duration::from_secs(10);
        while Instant::now() < stop_by && !rx.is_closed() {
            let _ = rx.try_recv();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(rx.is_closed(), "live Codex reader must stop before cleanup");
        assert!(completed, "live Codex did not finish within 180 seconds");
        assert_eq!(requests, 1);
        assert_eq!(
            response.trim(),
            "7391",
            "service must use the real answer without echoing it"
        );
        assert!(
            !fs::read_to_string(agent::transcript_path(&wire.id))
                .unwrap()
                .contains(SENTINEL)
        );
        fs::remove_dir(&path).unwrap();
        chat.update(cx, |c, cx| {
            c.retry_history(cx);
            c.retry_history(cx);
            assert!(c.history_save_error.is_none());
            assert!(c.completed_answers.is_empty());
        });
        let stored = fs::read_to_string(&path).unwrap();
        assert!(
            stored.contains(SENTINEL),
            "public answers must be preserved"
        );
        assert_eq!(
            stored.lines().count(),
            2,
            "retry must store the answer exactly once"
        );
        let history = agent::read_history(&path, u64::MAX);
        chat.update(cx, |c, cx| {
            let live = c
                .blocks
                .iter()
                .find_map(|b| match b {
                    Block::Tool(t) if t.name == "AskUserQuestion" => {
                        Some((t.input.clone(), t.result.clone(), t.summary.clone()))
                    }
                    _ => None,
                })
                .unwrap();
            assert!(live.2.contains(SENTINEL));
            c.blocks.clear();
            for ev in history {
                c.apply(ev, cx);
            }
            let Block::Tool(replayed) = &c.blocks[0] else {
                panic!("missing replayed answer")
            };
            assert_eq!(
                (
                    replayed.input.clone(),
                    replayed.result.clone(),
                    replayed.summary.clone()
                ),
                live
            );
        });
        fs::remove_file(path).unwrap();
        fs::remove_file(agent::transcript_path(&wire.id)).unwrap();
        let _ = fs::remove_file(
            crate::state::support_dir()
                .join("logs")
                .join(format!("{}.stderr.log", wire.id)),
        );
        fs::remove_dir_all(ws.dir()).unwrap();
    }

    #[test]
    fn answers_go_back_in_each_agents_shape() {
        let input = json!({"questions": [
            {"question": "Which DB?", "header": "DB", "multiSelect": false, "options": [{"label": "Postgres", "description": "SQL"}, {"label": "Redis", "description": ""}]},
            {"question": "Which tests?", "header": "Tests", "multiSelect": true, "options": [{"label": "Unit", "description": ""}, {"label": "E2E", "description": ""}]},
            {"question": "Name?", "header": "Name", "multiSelect": false, "options": [{"label": "a", "description": ""}, {"label": "b", "description": ""}]},
        ]});
        let qs = questions(&input);
        assert_eq!(
            (qs.len(), qs[1].multi, qs[0].other),
            (3, true, true),
            "claude always takes text"
        );
        assert_eq!(qs[0].options[0], ("Postgres".into(), "SQL".into()));
        let answers: Vec<Answer> = vec![
            (vec!["Postgres".into()], None),
            (vec!["Unit".into(), "E2E".into()], Some("smoke".into())),
            (vec![], None), // skipped
        ];
        let r = reply(&input, &qs, &answers, false);
        assert_eq!(
            r["questions"], input["questions"],
            "claude needs its questions back"
        );
        assert_eq!(
            r["answers"],
            json!({"Which DB?": "Postgres", "Which tests?": "Unit, E2E, smoke"})
        );
        assert_eq!(
            pairs(&qs, &answers),
            r#""Which DB?"="Postgres", "Which tests?"="Unit, E2E, smoke""#
        );

        let input = json!({"questions": [
            {"id": "file", "header": "File", "question": "Which file?", "isOther": true, "isSecret": false, "options": [{"label": "a.txt", "description": ""}]},
            {"id": "ok", "header": "Run", "question": "Run it?", "isOther": false, "isSecret": true, "options": [{"label": "Accept", "description": ""}]},
        ]});
        let qs = questions(&input);
        assert_eq!(
            (qs[0].other, qs[1].other, qs[1].secret),
            (true, false, true)
        );
        let answers: Vec<Answer> = vec![
            (vec![], Some("b.txt".into())),
            (vec!["Accept".into()], None),
        ];
        assert_eq!(
            pairs(&qs, &answers),
            r#""Which file?"="b.txt", "Run it?"="••••""#
        );
        assert_eq!(
            reply(&input, &qs, &answers, true),
            json!({"answers": {
                "file": {"answers": ["None of the above", "user_note: b.txt"]},
                "ok": {"answers": ["Accept"]},
            }})
        );
    }

    #[test]
    fn answer_summaries_keep_protocol_words_inside_user_text() {
        for (prefix, ending) in [
            (
                "Your questions have been answered: ",
                ". You can now continue",
            ),
            ("The user answered: ", ". Read the answers carefully"),
        ] {
            let summary = format!(r#""Instructions?"="Stop{ending} after tests""#);
            let result = format!("{prefix}{summary}{ending}");
            assert_eq!(answered(&result), Some(summary.as_str()));
        }
    }

    #[test]
    fn clipped_answer_summaries_keep_protocol_words_inside_user_text() {
        for (prefix, ending) in [
            (
                "Your questions have been answered: ",
                ". You can now continue",
            ),
            ("The user answered: ", ". Read the answers carefully"),
        ] {
            let summary = format!(
                r#""Instructions?"="Stop{ending} after {}""#,
                "x".repeat(agent::MAX_OUTPUT)
            );
            let result = format!("{prefix}{summary}{ending}");
            let record = json!({"type":"user", "message":{"content":[{"type":"tool_result", "tool_use_id":"q", "content":result}]}}).to_string();
            let events = agent::parse(&agent::compact(&record));
            let [Ev::ToolResult { text, .. }] = events.as_slice() else {
                panic!()
            };
            assert!(text.contains("… truncated,"));
            assert_eq!(
                answered(text).unwrap().len(),
                text.strip_prefix(prefix).unwrap().len()
            );
        }
    }

    /// The real CLIs, through Shark's own spawn, parse and reply: each asks and uses the answer
    /// it gets back (a pick, the user's own text), or ends its turn when told the user answers
    /// in the chat. Costs a few cents of the logged-in plans, so it runs only on request:
    /// `cargo test --features test-support live_questions -- --ignored --nocapture`
    #[cfg(feature = "test-support")]
    #[test]
    #[ignore]
    fn live_questions() {
        use crate::agent::{self, Ev};
        use crate::state::{ChatMeta, Workspace};
        // Unset: earlier tests in this process may have swapped in fakes.
        unsafe {
            std::env::remove_var("SHARK_CLAUDE");
            std::env::remove_var("SHARK_CODEX");
        }
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // `None`: the user closes the card.
        let ask = |model: &str,
                   plan: bool,
                   prompt: &str,
                   answer: Option<Answer>|
         -> (Vec<String>, String) {
            let tag = model.replace(':', "-");
            let root =
                std::env::temp_dir().join(format!("shark-live-{tag}-{}", std::process::id()));
            std::fs::create_dir_all(&root).unwrap();
            let ws = Workspace {
                name: format!("live-{tag}-{}", std::process::id()),
                ..Default::default()
            };
            let meta = ChatMeta {
                id: ws.name.clone(),
                model: Some(model.into()),
                effort: Some("low".into()),
                plan,
                ..Default::default()
            };
            std::fs::create_dir_all(crate::state::workspaces_dir()).unwrap();
            let _ = std::fs::remove_file(ws.dir());
            std::os::unix::fs::symlink(&root, ws.dir()).unwrap();
            let (mut a, rx) = agent::spawn(&ws, &meta).unwrap();
            if plan && !model.starts_with("codex:") {
                a.set_plan_mode(true).unwrap();
            }
            a.send(prompt).unwrap();
            let (mut seen, mut text) = (vec![], String::new());
            while let Ok(ev) = rx.recv_blocking() {
                match ev {
                    Ev::Permission {
                        request_id,
                        tool,
                        input,
                        ..
                    } if tool == "AskUserQuestion" => {
                        let qs = questions(&input);
                        seen.extend(qs.iter().map(|q| q.text.clone()));
                        let codex = model.starts_with("codex:");
                        match &answer {
                            Some(ans) => {
                                let r = reply(&input, &qs, &vec![ans.clone(); qs.len()], codex);
                                eprintln!("{model} asked {seen:?}, replying {r}");
                                a.allow(&request_id, r).unwrap();
                            }
                            // As `dismiss_asked` answers.
                            None if codex => {
                                let note =
                                    json!({"answers": [format!("user_note: {ANSWER_IN_CHAT}")]});
                                let by_id: serde_json::Map<String, Value> =
                                    qs.iter().map(|q| (q.id.clone(), note.clone())).collect();
                                a.allow(&request_id, json!({ "answers": by_id })).unwrap();
                            }
                            None => a.deny(&request_id, ANSWER_IN_CHAT).unwrap(),
                        }
                    }
                    Ev::ToolResult { text: t, .. } if seen.len() == 1 && answered(&t).is_some() => {
                        eprintln!("{model} tool result: {t}");
                    }
                    Ev::Text(t) => text.push_str(&t),
                    Ev::Plan { text: t, .. } => text.push_str(&t),
                    Ev::Result {
                        is_error, text: t, ..
                    } => {
                        assert!(!is_error, "{model}: {t}");
                        break;
                    }
                    Ev::Exited(e) => panic!("{model}: {e}"),
                    _ => {}
                }
            }
            drop(a);
            let _ = std::fs::remove_file(ws.dir());
            let _ = std::fs::remove_file(agent::transcript_path(&meta.id));
            (seen, text)
        };
        let prompt = "Use your tool for asking the user questions (AskUserQuestion, or \
            request_user_input) exactly once, with one question \"Which color?\" offering the \
            options Red and Blue. Then reply with only the color the user picked.";
        let pick: Answer = (vec!["Blue".into()], None);
        let own: Answer = (vec![], Some("Teal".into()));
        for (model, plan) in [("sonnet", false), ("codex:gpt-6.1-sol", true)] {
            for (answer, want) in [(Some(pick.clone()), "Blue"), (Some(own.clone()), "Teal")] {
                let (seen, text) = ask(model, plan, prompt, answer);
                eprintln!("{model} said: {text}");
                assert_eq!(seen.len(), 1, "{model} asked once: {seen:?}");
                assert!(
                    text.contains(want),
                    "{model} used the answer {want}: {text}"
                );
            }
            let (seen, text) = ask(model, plan, prompt, None);
            eprintln!("{model} after closing: {text}");
            assert_eq!(seen.len(), 1, "{model} doesn't ask again: {seen:?}");
            // It may ask again in plain text, but doesn't answer for the user.
            assert!(
                !matches!(text.trim(), "Blue" | "Red"),
                "{model} assumed: {text}"
            );
        }
    }

    #[test]
    fn rows_summarize_claudes_results() {
        let r = r#"Your questions have been answered: "Which DB?"="Postgres". You can now continue with these answers in mind."#;
        assert_eq!(answered(r), Some(r#""Which DB?"="Postgres""#));
        let r = r#"The user answered: "Name?"="bob". Read the answers carefully — they may request clarification"#;
        assert_eq!(answered(r), Some(r#""Name?"="bob""#));
        assert_eq!(answered(ANSWER_IN_CHAT), Some("Answered in the chat"));
        assert_eq!(answered("(interrupted)"), None);
        assert_eq!(
            asked_summary(&json!({"questions": [{"question": "A?"}, {"question": "B?"}]})),
            "A? · B?"
        );
    }
}
