//! Questions the agent asks the user (claude's AskUserQuestion, codex's requestUserInput),
//! answered in a card above the composer while the turn waits. The card is gpui-component's
//! questionnaire: number keys pick, ↑/↓ move, enter goes on, the last question submits.
use super::*;
use gpui_kit::component::Size;
use gpui_kit::component::input::InputState;
use gpui_kit::component::questionnaire::*;
use serde_json::json;

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
    if let Some(r) = result.strip_prefix("Your questions have been answered: ") {
        return r.split(". You can now continue").next();
    }
    if let Some(r) = result.strip_prefix("The user answered: ") {
        return r.split(". Read the answers carefully").next();
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
            |this, _, ev: &QuestionnaireEvent, window, cx| {
                if let QuestionnaireEvent::Submit(s) = ev {
                    this.answer_asked(s, window, cx);
                }
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
            let call = Call {
                id: asked.tool_use_id,
                name: "AskUserQuestion".into(),
                input: full_input(&asked.input),
                result: Some(summary.clone().into()),
                summary,
                is_error: false,
                open: false,
            };
            self.push(Block::Tool(call), cx);
        }
        cx.emit(ChatEvent::Asking);
        cx.notify();
    }

    /// The question card, just above the composer like the side-question card.
    pub(super) fn render_asked(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let asked = self.asked.as_ref()?;
        let form = asked.form.as_ref()?;
        let theme = cx.theme();
        let (focused, at) = (
            asked.focused(window, cx),
            form.read(cx).current_ix().unwrap_or(0),
        );
        let last = at + 1 == asked.qs.len();
        let q = &asked.qs[at.min(asked.qs.len() - 1)];
        let header = h_flex()
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
        let items = (asked.qs.iter().enumerate()).map(|(i, q)| {
            let name = i.to_string();
            QuestionnaireItem::new(form, name.clone())
                .child(QuestionnaireTitle::new(form, name.clone()))
                .child(
                    QuestionnaireChoices::new(form, name.clone()).children(
                        (0..q.options.len())
                            .map(|j| QuestionnaireChoice::new(form, name.clone(), j.to_string())),
                    ),
                )
                .child(QuestionnaireInput::new(form, name.clone()))
                .child(QuestionnaireError::new(form, name))
        });
        let actions = QuestionnaireActions::new(form)
            .child(QuestionnairePrevious::new(form))
            .child(QuestionnaireSkip::new(form))
            .child(QuestionnaireNext::new(form))
            .child(QuestionnaireSubmit::new(form));
        let faint = crate::app::faint(theme);
        let hints = h_flex()
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
                .mx_auto()
                .mb_2()
                .p_3()
                .gap_2()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(if focused { theme.primary } else { theme.border })
                .bg(theme.muted)
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
                        .children(items)
                        .child(actions),
                )
                .map(|d| match focused {
                    true => d.child(hints),
                    false => d.child(
                        h_flex()
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
