use super::*;
use std::time::Duration;

#[derive(Default)]
pub(super) struct Peek {
    pub task: Option<String>,
    pub home: bool,
    pub keyboard: bool,
    pub animate: bool,
    pub row: Option<String>,
    pub card: bool,
    timer: Option<gpui_kit::Task<()>>,
}

impl Shark {
    pub(super) fn close_peek(&mut self, cx: &mut Context<Self>) {
        self.peek = Peek::default();
        cx.notify();
    }

    pub(super) fn follow_peek(&mut self, task: Option<String>, home: bool) {
        if self.peek.task.is_some() {
            self.peek = Peek {
                task,
                home,
                keyboard: true,
                ..Default::default()
            };
        }
    }

    pub(super) fn toggle_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.peek.task.is_some() {
            return self.close_peek(cx);
        }
        let home = self.home.focus.is_focused(window);
        let task = if home {
            let rows = self.home_rows(cx);
            self.home_cursor(&rows)
                .map(|i| self.state.tasks[rows[i].ix].name.clone())
        } else if self.sidebar_focus.is_focused(window) {
            match self.sel {
                SideRow::Task(i) => self.state.tasks.get(i).map(|t| t.name.clone()),
                _ => None,
            }
        } else {
            None
        };
        self.peek = Peek {
            task,
            home,
            keyboard: true,
            ..Default::default()
        };
        cx.notify();
    }

    pub(super) fn peek_hover(
        &mut self,
        name: &str,
        home: bool,
        over: bool,
        cx: &mut Context<Self>,
    ) {
        if over {
            self.peek.timer = None;
            self.peek.row = Some(name.into());
            cx.notify();
            if self.peek.task.is_some() {
                self.peek.task = Some(name.into());
                self.peek.home = home;
                self.peek.animate = false;
                cx.notify();
            } else {
                let name = name.to_string();
                self.peek.timer = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(500))
                        .await;
                    this.update(cx, |s, cx| {
                        s.peek.task = Some(name);
                        s.peek.home = home;
                        s.peek.keyboard = false;
                        s.peek.animate = true;
                        cx.notify();
                    })
                    .ok();
                }));
            }
        } else if self.peek.row.as_deref() == Some(name) {
            self.peek.row = None;
            cx.notify();
            self.peek_leave(cx);
        }
    }

    pub(super) fn peek_leave(&mut self, cx: &mut Context<Self>) {
        self.peek.timer = None;
        if self.peek.keyboard || self.peek.card || self.peek.row.is_some() {
            return;
        }
        self.peek.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            this.update(cx, |s, cx| s.close_peek(cx)).ok();
        }));
    }
}

impl Shark {
    pub(super) fn render_peek(
        &self,
        task: &Task,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let facts = self.task_facts(task, cx);
        let muted = theme.muted_foreground;
        let section = |label: &str| {
            v_flex()
                .px_2()
                .py_2()
                .gap_1()
                .border_t_1()
                .border_color(theme.border)
                .child(div().text_xs().text_color(muted).child(label.to_string()))
        };
        let mut chats = section("Chats");
        for meta in &task.chats {
            let chat = self.chats.get(&meta.id).map(|c| c.read(cx));
            let agent = meta.agent.as_deref().and_then(|a| self.agent(a));
            let state = chat
                .filter(|c| c.asking())
                .map(|_| "Question to answer".to_string())
                .or_else(|| {
                    chat.filter(|c| c.plan_blocker(cx).is_none())
                        .map(|_| "Plan to approve".to_string())
                })
                .or_else(|| chat.and_then(|c| c.activity()))
                .or_else(|| chat.and_then(|c| c.failed.clone()))
                .unwrap_or_else(|| "Idle".into());
            chats = chats.child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Icon::new(IconName::Bot).with_size(px(14.)))
                            .child(div().flex_1().min_w_0().child(format!(
                                "{} · {}",
                                agent.map_or("Agent", |a| a.name.as_str()),
                                if meta.title.is_empty() {
                                    "New chat"
                                } else {
                                    &meta.title
                                }
                            ))),
                    )
                    .child(div().pl(px(22.)).text_xs().text_color(muted).child(format!(
                        "{} · {state}",
                        meta.model.as_deref().unwrap_or("Default model")
                    ))),
            );
        }
        let mut repos = section("Repositories");
        for saved in &task.repos {
            let repo = facts
                .repos
                .and_then(|rs| rs.iter().find(|r| r.name == saved.name));
            let branch = repo
                .and_then(|r| r.local.as_ref().ok())
                .filter(|l| !l.head.is_empty())
                .map_or(task.branch_of(saved), |l| {
                    l.branch.as_deref().unwrap_or("Detached HEAD")
                });
            let base = repo.map_or(saved.base.as_str(), |r| r.base.as_str());
            let mut details = v_flex()
                .id(SharedString::from(format!("peek-repo-{}", saved.name)))
                .role(gpui_kit::accesskit::Role::Group)
                .aria_label(format!("{}: {branch} → {base}", saved.name))
                .test_support()
                .gap_1()
                .child(
                    h_flex()
                        .gap_2()
                        .child(Icon::new(IconName::GitBranch).with_size(px(14.)))
                        .child(saved.name.clone()),
                )
                .child(
                    div()
                        .pl(px(22.))
                        .text_xs()
                        .text_color(muted)
                        .child(format!("{branch} → {base}")),
                );
            match repo.map(|r| &r.local) {
                Some(Ok(local)) if !local.head.is_empty() => {
                    let mut line =
                        format!("{} changed · {} behind base", local.changed, local.base.1);
                    if let Some((push, pull)) = local.remote {
                        line += &format!(" · {push} to push · {pull} to pull");
                    }
                    if local.conflicts > 0 {
                        line += &format!(" · {} conflicts", local.conflicts);
                    }
                    if let Some(op) = local.op {
                        line += &format!(" · {op:?} in progress");
                    }
                    details =
                        details.child(div().pl(px(22.)).text_xs().text_color(muted).child(line));
                    if let Some((added, removed)) = local.diff {
                        details = details.child(
                            h_flex()
                                .pl(px(22.))
                                .gap_2()
                                .text_xs()
                                .child(
                                    div()
                                        .text_color(theme.success_foreground)
                                        .child(format!("+{added}")),
                                )
                                .child(
                                    div()
                                        .text_color(theme.danger_foreground)
                                        .child(format!("−{removed}")),
                                ),
                        );
                    }
                }
                Some(Err(error)) => {
                    details = details.child(
                        div()
                            .text_xs()
                            .text_color(theme.danger_foreground)
                            .child(error.clone()),
                    );
                }
                _ => {
                    details =
                        details.child(div().text_xs().text_color(muted).child("Not checked yet"));
                }
            }
            repos = repos.child(details);
        }
        if task.repos.is_empty() {
            repos = repos.child("No repository");
        }
        let mut prs = section("Pull requests");
        let mut has_pr = false;
        for repo in facts.repos.into_iter().flatten() {
            if let Some(pr) = repo.pr() {
                has_pr = true;
                let checks = match &pr.checks {
                    crate::sync::Checks::Pending => "Checks running".into(),
                    crate::sync::Checks::Passing => "Checks passing".into(),
                    crate::sync::Checks::Failing(names) => {
                        format!("Checks failed: {}", names.join(", "))
                    }
                };
                prs = prs
                    .child(
                        h_flex()
                            .gap_2()
                            .child(pr.status().icon(theme).with_size(px(14.)))
                            .child(format!(
                                "{} · #{} · {}",
                                repo.name,
                                pr.number,
                                pr.status().label()
                            )),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(if matches!(pr.checks, crate::sync::Checks::Failing(_)) {
                                theme.danger_foreground
                            } else {
                                muted
                            })
                            .child(checks),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(match pr.review.as_str() {
                                "APPROVED" => "Approved",
                                "CHANGES_REQUESTED" => "Changes requested",
                                "REVIEW_REQUIRED" => "Review required",
                                _ => "No review decision",
                            }),
                    );
            } else if let Some(Err(reason)) = &repo.pr {
                has_pr = true;
                prs = prs.child(
                    div()
                        .text_xs()
                        .text_color(theme.danger_foreground)
                        .child(format!("{}: {reason}", repo.name)),
                );
            }
        }
        if let Some(age) = self.task_checked_age(&task.name, cx) {
            prs = prs.child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("Checked {}", crate::sync::ago(age))),
            );
        }
        let source = self
            .title_errors
            .get(&task.name)
            .map(|e| format!("Title failed · {e}"))
            .unwrap_or_else(|| {
                if self.titling.contains(&task.name) {
                    "Generating title…".into()
                } else {
                    self.title_sources
                        .get(&task.name)
                        .copied()
                        .unwrap_or("Saved title")
                        .into()
                }
            });
        let name = task.name.clone();
        let panel = menu::chrome(theme)
            .id("task-peek")
            .key_context("TaskPeek")
            .test_support()
            .role(gpui_kit::accesskit::Role::Group)
            .aria_label(format!("Details for {}", task.display_title()))
            .w(px(340.).min(window.viewport_size().width - px(16.)))
            .max_h(px(600.).min(window.viewport_size().height - px(16.)))
            .overflow_y_scroll()
            .text_sm()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|_, _, cx| cx.stop_propagation())
            .on_hover(cx.listener(|s, over: &bool, _, cx| {
                s.peek.card = *over;
                if *over {
                    s.peek.timer = None;
                } else {
                    s.peek_leave(cx);
                }
            }))
            .child(
                v_flex()
                    .px_2()
                    .py_2()
                    .gap_1()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(task.display_title().into_owned()),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(div().text_xs().text_color(muted).child(source))
                            .child(
                                Button::new("regenerate-peek-title")
                                    .link()
                                    .small()
                                    .label("Regenerate title")
                                    .on_click(cx.listener(move |s, _, w, cx| {
                                        s.regenerate_title(&name, w, cx)
                                    })),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .mx_1()
                    .p_2()
                    .gap_2()
                    .rounded(theme.radius)
                    .bg(theme.secondary)
                    .child(sidebar::indicator(facts.bucket, muted, px(14.), theme))
                    .child(div().min_w_0().child(facts.line)),
            )
            .child(chats)
            .child(repos)
            .when(has_pr, |d| d.child(prs))
            .when_some(task.primary_issue(), |d, issue| {
                d.child(
                    section("Linear issue")
                        .child(format!("{} · {}", issue.identifier, issue.status))
                        .child(issue.title.clone())
                        .child(format!("Priority: {}", issue.priority)),
                )
            })
            .when_some(self.runs.get(&task.name), |d, run| {
                d.child(section("Dev server").child(format!("localhost:{}", run.port)))
            })
            .child(
                div()
                    .border_t_1()
                    .border_color(theme.border)
                    .px_2()
                    .py_2()
                    .text_xs()
                    .text_color(muted)
                    .child("Space / Esc close · ↑ ↓ browse · Enter open"),
            );
        let panel = if self.peek.animate && !cx.reduce_motion() {
            panel
                .relative()
                .with_animation(
                    "peek-enter",
                    Animation::new(Duration::from_millis(150)).with_easing(ease_out_quint()),
                    |d, p| d.opacity(p).left(px(-4. * (1. - p))),
                )
                .into_any_element()
        } else {
            panel.into_any_element()
        };
        div()
            .absolute()
            .top_0()
            .left_full()
            .pl_2()
            .child(deferred(anchored().child(panel)))
            .into_any_element()
    }
}
