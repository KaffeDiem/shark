//! Background tasks as little animals: the activity line and its hover card.
use super::*;

pub(super) const CRITTERS: [IconName; 10] = [
    IconName::Cat,
    IconName::Bird,
    IconName::Rabbit,
    IconName::Squirrel,
    IconName::Turtle,
    IconName::Snail,
    IconName::Ghost,
    IconName::Fish,
    IconName::Dog,
    IconName::Rat,
];
pub(super) const PARADE_LIMIT: usize = 6;
pub(super) const HOP_INTERVAL: std::time::Duration = std::time::Duration::from_millis(400);

pub(super) fn assign_critters(critters: &mut HashMap<String, usize>, tasks: &[agent::Task]) {
    critters.retain(|id, _| tasks.iter().any(|t| &t.id == id));
    for task in tasks {
        if !critters.contains_key(&task.id) {
            let ix = (0..CRITTERS.len())
                .find(|ix| !critters.values().any(|used| used == ix))
                .unwrap_or(critters.len() % CRITTERS.len());
            critters.insert(task.id.clone(), ix);
        }
    }
}

pub(super) fn critter_icon(task: &agent::Task, ix: usize) -> IconName {
    if task.kind == "local_bash" {
        IconName::SquareTerminal
    } else {
        CRITTERS[ix]
    }
}

pub(super) fn critter_color(ix: usize, theme: &ThemeColor) -> Hsla {
    [
        theme.chart_1,
        theme.chart_2,
        theme.chart_3,
        theme.chart_4,
        theme.chart_5,
    ][ix % 5]
}

impl Chat {
    pub(super) fn set_background(&mut self, tasks: Vec<agent::Task>, cx: &mut Context<Self>) {
        assign_critters(&mut self.critters, &tasks);
        self.background = tasks;
        if self.background.is_empty() || cx.reduce_motion() || self.replaying {
            self.hopping = None;
            self.hop = 0;
        } else if self.hopping.is_none() {
            self.hopping = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(HOP_INTERVAL).await;
                    let keep_hopping = this.update(cx, |c, cx| {
                        if c.background.is_empty() || cx.reduce_motion() {
                            c.hopping = None;
                            return false;
                        }
                        c.hop = c.hop.wrapping_add(1);
                        cx.notify();
                        true
                    });
                    if !matches!(keep_hopping, Ok(true)) {
                        break;
                    }
                }
            }));
        }
    }

    /// The list's last item, under the newest message: a quiet line while a turn or background
    /// tasks run; hovering lists the details. Fixed height, so it comes and goes
    /// without remeasuring or moving the transcript.
    pub(super) fn render_activity(&self, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let line = div()
            .id("activity")
            .test_support()
            .w_full()
            .h(px(24.))
            .px_4();
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
        let critters = self.critters.clone();
        let visible = tasks.len().min(PARADE_LIMIT);
        line.child(
            div().w_full().max_w(px(680.)).mx_auto().child(
                HoverCard::new("activity-card")
                    .anchor(Anchor::BottomLeft)
                    .trigger(
                        h_flex()
                            .gap_1()
                            .h(px(24.))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .children(tasks.iter().take(PARADE_LIMIT).enumerate().map(|(i, t)| {
                                let ix = critters[&t.id];
                                // Fixed slots contain the hop without changing the footer's layout.
                                div()
                                    .id(("background-critter", i))
                                    .test_support()
                                    .size(px(12.))
                                    .flex_shrink_0()
                                    .child(
                                        Icon::new(critter_icon(t, ix))
                                            .with_size(px(12.))
                                            .text_color(critter_color(ix, theme))
                                            .when(
                                                !cx.reduce_motion() && i == self.hop % visible,
                                                |d| d.mt(px(-2.)),
                                            ),
                                    )
                            }))
                            .when(tasks.len() > PARADE_LIMIT, |d| {
                                d.child(format!("+{}", tasks.len() - PARADE_LIMIT))
                            })
                            // The shimmer redraws the chat every frame, as a tool's spinner
                            // does; only during a turn, not for tasks left running after it.
                            .map(|d| match busy {
                                true => d
                                    .child(Spinner::new().with_size(px(12.)))
                                    .child(ShimmerText::new(text).id("activity-text")),
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
                                    let ix = critters[&t.id];
                                    h_flex()
                                        .gap_1p5()
                                        .child(
                                            Icon::new(critter_icon(t, ix))
                                                .with_size(px(12.))
                                                .text_color(critter_color(ix, theme)),
                                        )
                                        .child(t.description.clone())
                                }))
                            })
                    }),
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin

    #[test]
    fn critters_stay_with_tasks_and_reuse_freed_icons() {
        let task = |id: &str| agent::Task {
            id: id.into(),
            kind: "local_agent".into(),
            description: "Same description".into(),
        };
        let mut critters = HashMap::new();
        assign_critters(&mut critters, &[task("a"), task("b"), task("c")]);
        assert_eq!((critters["a"], critters["b"], critters["c"]), (0, 1, 2));
        assert_ne!(critter_icon(&task("a"), 0), critter_icon(&task("b"), 1));

        // Reordering and finishing a neighbor never change a survivor's icon.
        assign_critters(&mut critters, &[task("c"), task("a")]);
        assert_eq!((critters["a"], critters["c"]), (0, 2));
        assert!(!critters.contains_key("b"));
        assign_critters(&mut critters, &[task("c"), task("d"), task("a")]);
        assert_eq!(critters["d"], 1);

        let shell = agent::Task {
            kind: "local_bash".into(),
            ..task("shell")
        };
        assert_eq!(critter_icon(&shell, 2), IconName::SquareTerminal);
        assign_critters(&mut critters, &[]);
        assert!(critters.is_empty());
    }

    #[test]
    fn critters_wrap_when_the_catalog_is_full() {
        let tasks: Vec<_> = (0..CRITTERS.len() * 2 + 1)
            .map(|i| agent::Task {
                id: i.to_string(),
                kind: "local_agent".into(),
                description: String::new(),
            })
            .collect();
        let mut critters = HashMap::new();
        assign_critters(&mut critters, &tasks);
        for (i, t) in tasks.iter().enumerate() {
            assert_eq!(critters[&t.id], i % CRITTERS.len());
        }
        let kept = critters.clone();
        assign_critters(&mut critters, &tasks[1..]);
        for t in &tasks[1..] {
            assert_eq!(critters[&t.id], kept[&t.id]);
        }
    }
}
