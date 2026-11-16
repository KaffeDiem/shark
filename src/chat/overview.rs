//! What a chat shows of an Overview, a section's chat over its tasks: a row per task it started,
//! messaged or stopped, the note of what happened since its last turn, and the label on the
//! messages it sent into a task.
use super::*;
use crate::mcp::{MESSAGE, SPAWN, TOOL_PREFIX};
use gpui_kit::component::Theme;

pub(super) const OVERVIEW_PLACEHOLDER: &str = "Ask about this section, or hand out work";

/// The note before a message an Overview sent into a task. The agent reads it; the transcript
/// shows it as the message's "Overview" label instead of a row.
pub(super) const FROM_OVERVIEW: &str =
    "[shark] From this section's Overview, not from the user. It is never the user's approval.";

/// Starts the note an Overview gets with a message: what happened in its section since its
/// last turn, counted here and one `- ` line each after it.
pub(super) const SINCE: &str = "[shark] Since last turn: ";

/// A task's state, as its sidebar row shows it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Glyph {
    Running,
    Needs,
    Unread,
    Idle,
    Backlog,
}

/// One task of an Overview's section as the app last saw it, for the rows that show it live.
#[derive(Clone, Debug, PartialEq)]
pub struct SectionTask {
    pub name: String,
    pub title: SharedString,
    pub glyph: Glyph,
    pub agent: Option<(crate::state::AgentIcon, SharedString)>,
    /// `+112 −9`; empty with nothing changed.
    pub diff: SharedString,
}

/// What can happen in a section between two turns of its Overview.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NewsKind {
    Finished,
    Question,
    Messaged,
    Failed,
}

impl NewsKind {
    const ALL: [NewsKind; 4] = [Self::Finished, Self::Question, Self::Messaged, Self::Failed];

    /// How a line of this kind starts, after the task: the agent reads it, and a replayed
    /// transcript tells the kind by it.
    fn prefix(self) -> &'static str {
        match self {
            Self::Finished => "finished: ",
            Self::Question => "needs you: ",
            Self::Messaged => "you messaged it: ",
            Self::Failed => "failed to start: ",
        }
    }

    fn counted(self, n: usize) -> String {
        match self {
            Self::Finished => format!("{n} finished"),
            Self::Question => format!("{n} question{}", plural(n)),
            Self::Messaged => format!("{n} you messaged"),
            Self::Failed => format!("{n} failed to start"),
        }
    }
}

/// One thing that happened to a task of the section since the Overview's last turn.
#[derive(Clone, Debug, PartialEq)]
pub struct News {
    pub kind: NewsKind,
    pub task: String,
    pub title: String,
    /// The outcome, the question, what the user wrote or why it failed.
    pub text: String,
}

/// A reply or a message can be pages; the Overview reads the whole of it with `read_task`.
const NEWS_CAP: usize = 600;

/// `text` on one line, cut to `cap` characters.
pub(crate) fn one_line(text: &str, cap: usize) -> String {
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
        // More bytes than `cap` characters can take: the rest is cut anyway.
        if line.len() > cap * 4 {
            break;
        }
    }
    if let Some((at, _)) = line.char_indices().nth(cap) {
        line.truncate(at);
        line.push('…');
    }
    line
}

/// The note for `news`, oldest first.
pub(super) fn since_note(news: &[News]) -> String {
    let counts: Vec<String> = (NewsKind::ALL.iter())
        .filter_map(|k| {
            let n = news.iter().filter(|x| x.kind == *k).count();
            (n > 0).then(|| k.counted(n))
        })
        .collect();
    let mut note = format!("{SINCE}{}", counts.join(", "));
    for n in news {
        // One line each, and no title that reads as the separator after it.
        let title = one_line(&n.title, 120).replace(" [task ", " [task: ");
        let text = one_line(&n.text, NEWS_CAP);
        note += &format!("\n- {title} [task {}]: {}{text}", n.task, n.kind.prefix());
    }
    note
}

/// A line of the note taken apart again: the task, its title then, the kind and the rest.
fn since_row(line: &str) -> Option<(&str, &str, NewsKind, &str)> {
    let (title, rest) = line.strip_prefix("- ")?.split_once(" [task ")?;
    let (task, text) = rest.split_once("]: ")?;
    let kind = (NewsKind::ALL.into_iter()).find(|k| text.starts_with(k.prefix()))?;
    Some((task, title, kind, &text[kind.prefix().len()..]))
}

/// The rows context notes show as: their `[shark]` lines, without what a note goes on to quote.
/// The Overview's note keeps its lines, to open; its label on a message is no row at all.
pub(super) fn info_notes(notes: &str) -> Vec<Block> {
    let mut blocks = vec![];
    for line in notes.lines() {
        if let Some(counts) = line.strip_prefix(SINCE) {
            blocks.push(Block::Since {
                head: format!("Since last turn · {}", counts.replace(", ", " · ")).into(),
                rows: vec![],
                open: false,
            });
        } else if line.starts_with("[shark] ") {
            blocks.push(Block::Info(line.into()));
        } else if let Some(Block::Since { rows, .. }) = blocks.last_mut()
            && line.starts_with("- ")
        {
            rows.push(line.into());
        }
    }
    blocks
}

/// The tool an Overview's call that acts on a task used: such calls group by tool, as
/// "Started 3 tasks" over a row per task.
pub(super) fn act(name: &str) -> Option<&str> {
    crate::mcp::acts(name).then(|| &name[TOOL_PREFIX.len()..])
}

/// A call's one line, when it is one of Shark's tools: what its row shows before the task is
/// known (the brief's first line), or next to it (the message).
pub(super) fn summarize(name: &str, input: &Value) -> Option<String> {
    let text = |field: &str| input[field].as_str().unwrap_or_default();
    Some(match name.strip_prefix(TOOL_PREFIX)? {
        SPAWN => title_of(text("brief")),
        // The row has room for more of a message than a title's forty characters.
        MESSAGE => one_line(text("message"), 200),
        _ => text("task").to_string(),
    })
}

/// A row of a task list: who, how it is doing, and a way there.
struct TaskRow {
    /// Opens on click.
    task: Option<String>,
    icon: AnyElement,
    title: SharedString,
    note: Option<(SharedString, Hsla)>,
    agent: Option<(crate::state::AgentIcon, SharedString)>,
    diff: SharedString,
}

fn glyph(glyph: Option<Glyph>, theme: &Theme) -> AnyElement {
    let size = px(16.);
    let (icon, color) = match glyph {
        Some(Glyph::Running) => {
            return Spinner::new()
                .icon(IconName::LoaderCircle)
                .color(theme.primary)
                .with_size(size)
                .into_any_element();
        }
        Some(Glyph::Needs) => (IconName::CircleAlert, theme.warning_foreground),
        Some(Glyph::Unread) => (IconName::CircleCheck, theme.success_foreground),
        Some(Glyph::Idle) => (IconName::CircleDashed, theme.muted_foreground),
        Some(Glyph::Backlog) => (IconName::CirclePause, theme.muted_foreground),
        // Stopped, or no longer a task of this section.
        None => (IconName::CircleX, crate::ui::faint(theme)),
    };
    (Icon::new(icon).with_size(size).text_color(color)).into_any_element()
}

impl Chat {
    /// The app's latest look at the Overview's section; the rows redraw when it changed.
    pub fn set_section(&mut self, tasks: Vec<SectionTask>, cx: &mut Context<Self>) {
        if self.section != tasks {
            self.section = tasks;
            cx.notify();
        }
    }

    /// Something happened in the Overview's section: its next message says so. The latest of a
    /// kind per task is enough.
    pub fn add_news(&mut self, news: News) {
        (self.news).retain(|n| n.task != news.task || n.kind != news.kind);
        self.news.push(news);
    }

    /// A message from the section's Overview. It goes out at once, into a running turn too,
    /// marked so that the agent and the transcript know it isn't the user's. Returns whether
    /// it went out.
    pub fn tell(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        self.from_overview = true;
        let sent = self.send_text(text.trim().to_string(), true, cx);
        self.from_overview = false;
        sent
    }

    /// The notes only this message carries, after the task's own (`pending_context`): with
    /// `news`, what happened in an Overview's section; and that an Overview wrote it.
    pub(super) fn message_notes(&self, news: bool) -> Vec<String> {
        let mut notes = vec![];
        if news {
            notes.push(since_note(&self.news));
        }
        if self.from_overview {
            notes.push(FROM_OVERVIEW.into());
        }
        notes
    }

    /// The messages the section's Overview wrote into this chat.
    #[cfg(all(test, feature = "test-support"))]
    pub(crate) fn overview_messages(&self) -> Vec<String> {
        ((0..self.blocks.len()).filter(|&bi| self.from_overview(bi)))
            .filter_map(|bi| self.blocks[bi].message().map(|(t, _)| t.to_string()))
            .collect()
    }

    /// What waits for this Overview's next message: the kind and the task.
    #[cfg(all(test, feature = "test-support"))]
    pub(crate) fn pending_news(&self) -> Vec<(NewsKind, String)> {
        (self.news.iter())
            .map(|n| (n.kind, n.task.clone()))
            .collect()
    }

    /// The notes this Overview got: each one's line and its rows.
    #[cfg(all(test, feature = "test-support"))]
    pub(crate) fn since_notes(&self) -> Vec<(String, Vec<String>)> {
        (self.blocks.iter())
            .filter_map(|b| match b {
                Block::Since { head, rows, .. } => Some((head.to_string(), rows.clone())),
                _ => None,
            })
            .collect()
    }

    /// A transcript for the README's screenshot: an Overview that started `tasks`, was told
    /// what happened to them and messaged one.
    #[cfg(all(test, feature = "test-support"))]
    pub(crate) fn demo_overview(&mut self, tasks: [(&str, &str); 2], cx: &mut Context<Self>) {
        use crate::harness::Ev;
        let tool = |id: &str, name: &str, input: Value| Ev::ToolUse {
            id: id.into(),
            name: format!("{TOOL_PREFIX}{name}"),
            input,
        };
        let answer = |id: &str, task: &str| Ev::ToolResult {
            id: id.into(),
            text: serde_json::json!({"task": task}).to_string(),
            is_error: false,
        };
        let done = |secs| Ev::Result {
            is_error: false,
            text: String::new(),
            secs,
        };
        let [(first, first_title), (second, second_title)] = tasks;
        let news = |kind, task: &str, title: &str, text: &str| News {
            kind,
            task: task.into(),
            title: title.into(),
            text: text.into(),
        };
        let since = since_note(&[
            news(
                NewsKind::Finished,
                first,
                first_title,
                "its turn ended, +38 −4",
            ),
            news(
                NewsKind::Messaged,
                second,
                second_title,
                "Use the theme token, not a hex value.",
            ),
        ]);
        let events = [
            Ev::User("Split the login work into two tasks and start them.".into()),
            Ev::Text("Two tasks. The settings screen builds on the rate limit, so it starts from that branch.".into()),
            tool("a", SPAWN, serde_json::json!({"brief": first_title})),
            tool("b", SPAWN, serde_json::json!({"brief": second_title})),
            answer("a", first),
            answer("b", second),
            done(48),
            Ev::User(format!("{since}{NOTES_END}The rate limit looks right. Carry on.")),
            tool("r", crate::mcp::READ, serde_json::json!({})),
            Ev::ToolResult {
                id: "r".into(),
                text: "{}".into(),
                is_error: false,
            },
            Ev::Text("The settings screen can continue from the pushed parent.".into()),
            tool("m", MESSAGE, serde_json::json!({"task": second, "message": "Start from the pushed parent."})),
            answer("m", second),
            done(31),
        ];
        for ev in events {
            self.play(ev, cx);
        }
        self.busy = false;
    }

    /// Whether block `bi` is the label of the message after it, which shows it instead.
    pub(super) fn overview_label(&self, bi: usize) -> bool {
        matches!(&self.blocks[bi], Block::Info(t) if t == FROM_OVERVIEW)
            && matches!(self.blocks.get(bi + 1), Some(Block::User(..)))
    }

    /// Whether the message in block `bi` came from the section's Overview.
    pub(super) fn from_overview(&self, bi: usize) -> bool {
        bi.checked_sub(1).is_some_and(|b| self.overview_label(b))
    }

    /// "Overview", above a message's text.
    pub(super) fn overview_tag(theme: &Theme) -> Div {
        h_flex()
            .gap(px(5.))
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(Icon::new(IconName::MessagesSquare).with_size(px(12.)))
            .child("Overview")
    }

    fn task_list(
        &self,
        key: (&'static str, usize),
        rows: Vec<TaskRow>,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = cx.theme();
        let (border, hover, muted, faint) = (
            theme.border,
            theme.list_hover,
            theme.muted_foreground,
            crate::ui::faint(theme),
        );
        let small = px(11.25);
        v_flex()
            .rounded(theme.radius)
            .border_1()
            .border_color(border)
            .overflow_hidden()
            .text_sm()
            .children(rows.into_iter().enumerate().map(|(n, r)| {
                let opens = r.task.is_some();
                let row =
                    h_flex()
                        .id((SharedString::from(format!("{}-{}", key.0, key.1)), n))
                        .h(px(32.))
                        .px(px(10.))
                        .gap_2()
                        .when(n > 0, |d| d.border_t_1().border_color(border))
                        .child(r.icon)
                        .child(
                            div()
                                .flex_none()
                                .max_w(relative(0.6))
                                .truncate()
                                .child(r.title),
                        )
                        .child(div().flex_1().min_w_0().truncate().text_xs().map(
                            |d| match r.note {
                                Some((note, color)) => d.text_color(color).child(note),
                                None => d,
                            },
                        ))
                        .when_some(r.agent, |d, (icon, name)| {
                            d.child(
                                h_flex()
                                    .flex_none()
                                    .gap_1()
                                    .text_size(small)
                                    .text_color(muted)
                                    .child(
                                        crate::app::agent_icon_view::agent_icon(&icon)
                                            .size(px(14.)),
                                    )
                                    .child(name),
                            )
                        })
                        .when(!r.diff.is_empty(), |d| {
                            d.child(
                                div()
                                    .flex_none()
                                    .text_size(small)
                                    .text_color(muted)
                                    .child(r.diff),
                            )
                        });
                match r.task {
                    Some(task) => row
                        .hover(move |d| d.bg(hover))
                        .cursor_pointer()
                        .child(
                            Icon::new(IconName::ChevronRight)
                                .with_size(px(12.))
                                .text_color(faint),
                        )
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.stop_propagation();
                            cx.emit(ChatEvent::OpenTask(task.clone()));
                        })),
                    None => row,
                }
                .when(!opens, |d| d.text_color(muted))
            }))
    }

    /// The row of one of the section's tasks as it is now.
    fn live_row(
        &self,
        task: &SectionTask,
        note: Option<(SharedString, Hsla)>,
        theme: &Theme,
    ) -> TaskRow {
        TaskRow {
            task: Some(task.name.clone()),
            icon: glyph(Some(task.glyph), theme),
            title: task.title.clone(),
            note,
            agent: task.agent.clone(),
            diff: task.diff.clone(),
        }
    }

    /// The Overview's calls of one tool that acts on tasks: what it did, over a row per task
    /// showing how that task is doing now.
    pub(super) fn render_acts(&self, bi: usize, calls: &[Call], cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let tool = act(&calls[0].name).unwrap_or_default();
        let (n, s) = (calls.len(), plural(calls.len()));
        let label = match tool {
            SPAWN => format!("Started {n} task{s}"),
            MESSAGE => format!("Messaged {n} task{s}"),
            _ => format!("Stopped {n} task{s}"),
        };
        let muted = theme.muted_foreground;
        let plain = |icon: AnyElement, c: &Call, note: Option<(SharedString, Hsla)>| TaskRow {
            task: None,
            icon,
            title: c.summary.clone().into(),
            note,
            agent: None,
            diff: SharedString::default(),
        };
        let rows = (calls.iter())
            .map(|c| {
                let Some(result) = &c.result else {
                    let spinner = Spinner::new().with_size(px(16.)).into_any_element();
                    return plain(spinner, c, None);
                };
                if c.is_error {
                    let x = Icon::new(IconName::CircleX)
                        .with_size(px(16.))
                        .text_color(theme.danger_foreground);
                    let why = (one_line(result, 200).into(), theme.danger_foreground);
                    return plain(x.into_any_element(), c, Some(why));
                }
                // An interrupted call's result is no answer of Shark's: show what it says.
                let Ok(answer) = serde_json::from_str::<Value>(result) else {
                    let said = (one_line(result, 200).into(), muted);
                    return plain(glyph(None, theme), c, Some(said));
                };
                let name = answer["task"].as_str();
                match name.and_then(|n| self.section.iter().find(|t| t.name == n)) {
                    Some(task) => {
                        let said = (tool == MESSAGE).then(|| (c.summary.clone().into(), muted));
                        let mut row = self.live_row(task, said, theme);
                        // A task it stopped reads as stopped until it does something again.
                        if tool == crate::mcp::STOP && task.glyph == Glyph::Idle {
                            row.icon = glyph(None, theme);
                        }
                        row
                    }
                    None => plain(
                        glyph(None, theme),
                        c,
                        Some(("no longer in this section".into(), muted)),
                    ),
                }
            })
            .collect();
        v_flex()
            .gap(px(6.))
            .py_1()
            .child(div().text_sm().text_color(muted).child(label))
            .child(self.task_list(("acts", bi), rows, cx))
    }

    /// The note the Overview got with a message: one muted line, open to what it was told.
    pub(super) fn render_since(
        &self,
        bi: usize,
        head: &SharedString,
        rows: &[String],
        open: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = cx.theme();
        let (muted, warn, bad, ok) = (
            theme.muted_foreground,
            theme.warning_foreground,
            theme.danger_foreground,
            theme.success_foreground,
        );
        let chevron = match open {
            true => IconName::ChevronDown,
            false => IconName::ChevronRight,
        };
        let line = h_flex()
            .gap(px(5.))
            .text_xs()
            .text_color(muted)
            .child(head.clone())
            .child(Icon::new(chevron).with_size(px(12.)));
        if !open {
            return v_flex().child(line);
        }
        let icon = |name: IconName, color: Hsla| {
            (Icon::new(name).with_size(px(16.)).text_color(color)).into_any_element()
        };
        let rows = (rows.iter())
            .map(|raw| {
                let Some((task, title, kind, text)) = since_row(raw) else {
                    let dash = icon(IconName::CircleDashed, muted);
                    return TaskRow {
                        task: None,
                        icon: dash,
                        title: raw.clone().into(),
                        note: None,
                        agent: None,
                        diff: SharedString::default(),
                    };
                };
                let (icon, color) = match kind {
                    NewsKind::Finished => (icon(IconName::CircleCheck, ok), muted),
                    NewsKind::Question => (icon(IconName::CircleAlert, warn), warn),
                    NewsKind::Messaged => (icon(IconName::CircleDashed, muted), muted),
                    NewsKind::Failed => (icon(IconName::CircleX, bad), bad),
                };
                let text = match kind {
                    NewsKind::Messaged => format!("You: “{text}”"),
                    _ => text.to_string(),
                };
                TaskRow {
                    // A task that left the section since has no page to open from here.
                    task: (self.section.iter().any(|t| t.name == task)).then(|| task.to_string()),
                    icon,
                    title: title.to_string().into(),
                    note: Some((text.into(), color)),
                    agent: None,
                    diff: SharedString::default(),
                }
            })
            .collect();
        v_flex()
            .gap(px(6.))
            .child(line)
            .child(self.task_list(("since", bi), rows, cx))
    }

    /// An Overview before its first message: the section's tasks, just above the composer.
    pub(super) fn section_list(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let rows: Vec<_> = (self.section.iter())
            .map(|t| self.live_row(t, None, theme))
            .collect();
        let empty = rows.is_empty();
        let hint = div()
            .text_sm()
            .text_color(crate::ui::faint(theme))
            .child("No tasks in this section yet. Ask for some, or move tasks in.");
        v_flex().absolute().inset_0().justify_end().pb_3().child(
            div()
                .w_full()
                .max_w(px(680.))
                .mx_auto()
                .map(|d| match empty {
                    true => d.child(hint),
                    false => d.child(self.task_list(("section", 0), rows, cx)),
                }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin

    fn news(kind: NewsKind, task: &str, title: &str, text: &str) -> News {
        News {
            kind,
            task: task.into(),
            title: title.into(),
            text: text.into(),
        }
    }

    #[test]
    fn the_note_counts_each_kind_and_reads_back() {
        let note = since_note(&[
            news(
                NewsKind::Finished,
                "push-eel",
                "Push parents first",
                "ended. +76 −8.\nLast reply: Done.",
            ),
            news(
                NewsKind::Question,
                "rail-cod",
                "Sidebar rail colours",
                "Question to answer",
            ),
            news(
                NewsKind::Messaged,
                "retarget-ray",
                "Retarget on merge",
                "Use the theme token.",
            ),
            news(
                NewsKind::Finished,
                "b",
                "Title [task x]: finished:\nodd",
                "ended",
            ),
        ]);
        let mut lines = note.lines();
        assert_eq!(
            lines.next(),
            Some("[shark] Since last turn: 2 finished, 1 question, 1 you messaged")
        );
        assert_eq!(
            lines.next(),
            Some(
                "- Push parents first [task push-eel]: finished: ended. +76 −8. Last reply: Done."
            )
        );
        // As the transcript shows it, live and replayed: one line to open, and no row for
        // what the lines quote.
        let blocks = info_notes(&format!(
            "[shark] Repositories changed\n- api\n{note}\n{FROM_OVERVIEW}"
        ));
        let [
            Block::Info(first),
            Block::Since {
                head,
                rows,
                open: false,
            },
            Block::Info(last),
        ] = &blocks[..]
        else {
            panic!("{} blocks", blocks.len());
        };
        assert_eq!(first, "[shark] Repositories changed");
        assert_eq!(last, FROM_OVERVIEW);
        assert_eq!(
            head,
            "Since last turn · 2 finished · 1 question · 1 you messaged"
        );
        assert_eq!(rows.len(), 4);
        assert_eq!(
            since_row(&rows[1]),
            Some((
                "rail-cod",
                "Sidebar rail colours",
                NewsKind::Question,
                "Question to answer"
            ))
        );
        assert_eq!(
            since_row(&rows[2]),
            Some((
                "retarget-ray",
                "Retarget on merge",
                NewsKind::Messaged,
                "Use the theme token."
            ))
        );
        // A title that reads like the separator, over two lines, is still one row of its task.
        assert_eq!(
            since_row(&rows[3]),
            Some((
                "b",
                "Title [task: x]: finished: odd",
                NewsKind::Finished,
                "ended"
            ))
        );
        assert_eq!(since_row("- no task here"), None);
    }

    #[test]
    fn long_text_is_one_capped_line() {
        assert_eq!(one_line("a\n\n b\tc ", 10), "a b c");
        assert_eq!(one_line("abcdef ghij", 6), "abcdef…");
        assert_eq!(one_line("abcdefgh", 4), "abcd…");
        assert_eq!(one_line("", 4), "");
        let long = "word ".repeat(1000);
        assert_eq!(one_line(&long, NEWS_CAP).chars().count(), NEWS_CAP + 1);
    }

    #[test]
    fn calls_that_act_on_tasks_group_by_tool() {
        assert_eq!(act("mcp__shark__spawn_task"), Some("spawn_task"));
        assert_eq!(act("mcp__shark__stop_task"), Some("stop_task"));
        assert_eq!(
            act("mcp__shark__read_task"),
            None,
            "reads group with other tools"
        );
        assert_eq!(act("Bash"), None);
        let input = serde_json::json!({"brief": "Restack after squash merge\n\nDetails…", "task": "t", "message": "Carry on."});
        assert_eq!(
            summarize("mcp__shark__spawn_task", &input).as_deref(),
            Some("Restack after squash merge")
        );
        assert_eq!(
            summarize("mcp__shark__message_task", &input).as_deref(),
            Some("Carry on.")
        );
        assert_eq!(
            summarize("mcp__shark__stop_task", &input).as_deref(),
            Some("t")
        );
        assert_eq!(summarize("Bash", &input), None);
    }
}
