//! Recent-first task navigation. PR refreshes repaint rows without reranking them.
use super::interaction::now_ms;
use super::*;
use crate::sync::Repo as SyncRepo;
use std::cmp::Reverse;

pub(super) struct TaskPicker {
    pub(super) state: Entity<CommandState>,
    tasks: Vec<PickerTask>,
    pub(super) hits: Vec<String>,
    query: String,
    current: Option<String>,
    shark: WeakEntity<Shark>,
    select: Option<Option<usize>>,
    _updates: Subscription,
    sync_updates: HashMap<EntityId, Subscription>,
    _escape: Subscription,
    command_dirty: bool,
    viewport: Size<Pixels>,
    minute: u64,
}

#[derive(PartialEq)]
pub(super) struct PickerTask {
    name: String,
    title: String,
    repos: Vec<(String, (String, IconName, Option<crate::prs::PrStatus>))>,
    last_interacted_at: Option<u64>,
    running: usize,
    attention: Attn,
}

impl PickerTask {
    fn new(ws: &Task, repos: Option<&[SyncRepo]>, running: usize, attention: Attn) -> Self {
        Self {
            name: ws.name.clone(),
            title: ws.display_title().to_string(),
            repos: ws
                .repos
                .iter()
                .map(|r| {
                    (
                        r.name.clone(),
                        status(
                            repos.and_then(|repos| repos.iter().find(|repo| repo.name == r.name)),
                        ),
                    )
                })
                .collect(),
            last_interacted_at: ws.last_interacted_at,
            running,
            attention,
        }
    }

    fn activity(&self) -> Option<String> {
        let running = match self.running {
            0 => None,
            1 => Some("1 chat running".to_string()),
            n => Some(format!("{n} chats running")),
        };
        match (&self.attention, running) {
            (Attn::Needs(step), Some(running)) => Some(format!("{running} · {step}")),
            (Attn::Needs(step), None) => Some(step.clone()),
            (_, Some(running)) => Some(running),
            (Attn::Unread, None) => Some("Unread".into()),
            (Attn::None, None) => None,
        }
    }
}

fn search_key(name: &str) -> String {
    name.to_lowercase()
        .split(|c: char| c.is_whitespace() || matches!(c, '-' | '_' | '/' | '.'))
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn rank(tasks: &[PickerTask], query: &str) -> Vec<String> {
    let q: Vec<char> = search_key(query.trim()).chars().collect();
    let mut hits: Vec<_> = tasks
        .iter()
        .enumerate()
        .filter_map(|(order, ws)| {
            let score = [ws.title.as_str(), ws.name.as_str()]
                .into_iter()
                .chain(ws.repos.iter().map(|(name, _)| name.as_str()))
                .filter_map(|name| crate::complete::score(&q, &search_key(name), 0))
                .max()?;
            Some((
                Reverse(score),
                Reverse(ws.last_interacted_at),
                order,
                ws.name.clone(),
            ))
        })
        .collect();
    hits.sort();
    hits.into_iter().map(|(_, _, _, name)| name).collect()
}

/// Lifecycle is separate from the next Git action: failing checks must not hide Draft.
fn status(repo: Option<&SyncRepo>) -> (String, IconName, Option<crate::prs::PrStatus>) {
    match repo.and_then(|r| r.pr.as_ref()) {
        None => ("Not checked".into(), IconName::Clock, None),
        Some(Err(_)) => ("Status unavailable".into(), IconName::CircleAlert, None),
        Some(Ok(None)) => ("No PR".into(), IconName::GitBranch, None),
        Some(Ok(Some(pr))) => {
            let state = pr.status();
            let label = state.label();
            (
                format!(
                    "{}{} #{}",
                    label[..1].to_uppercase(),
                    &label[1..],
                    pr.number
                ),
                IconName::GitPullRequest,
                Some(state),
            )
        }
    }
}

fn interaction_time(at: Option<u64>, now: u64) -> (String, String) {
    let Some(at) = at else {
        return (
            "No recorded interaction".into(),
            "No recorded interaction".into(),
        );
    };
    let age = now.saturating_sub(at);
    let relative = if age < 60_000 {
        "Just now".into()
    } else {
        crate::sync::ago(std::time::Duration::from_millis(age))
    };
    let date = objc2_foundation::NSDate::dateWithTimeIntervalSince1970(at as f64 / 1000.);
    (
        relative,
        format!("Last interaction: {}", date.description()),
    )
}

impl TaskPicker {
    pub(super) fn snapshot(s: &Shark, cx: &App) -> Vec<PickerTask> {
        s.ws_order(true)
            .into_iter()
            .map(|ix| &s.state.tasks[ix])
            .filter(|ws| !s.archiving.contains(&ws.name))
            .map(|ws| {
                PickerTask::new(
                    ws,
                    s.overview_repos(&ws.name, cx),
                    s.running_chats(&ws.name, cx),
                    s.attention_of(&ws.name, cx),
                )
            })
            .collect()
    }

    pub(super) fn new(
        tasks: Vec<PickerTask>,
        current: Option<String>,
        shark: WeakEntity<Shark>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let state = cx.new(|cx| CommandState::new(window, cx));
        let updates = cx.observe(&shark.upgrade().unwrap(), |p, _, cx| p.refresh(cx));
        cx.defer_in(window, |p, _, cx| p.refresh(cx));
        let focus = state.focus_handle(cx);
        let escape = cx.intercept_keystrokes(move |ev, w, cx| {
            if ev.keystroke.key == "escape" && focus.contains_focused(w, cx) {
                w.close_dialog(cx);
                cx.stop_propagation();
            }
        });
        let mut picker = Self {
            state,
            tasks,
            hits: vec![],
            query: String::new(),
            current,
            shark,
            select: None,
            sync_updates: HashMap::new(),
            _updates: updates,
            _escape: escape,
            command_dirty: true,
            viewport: window.viewport_size(),
            minute: now_ms() / 60_000,
        };
        picker.rank();
        picker
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(shark) = self.shark.upgrade() else {
            return;
        };
        let syncs = shark.read(cx).syncs.values().cloned().collect::<Vec<_>>();
        for sync in &syncs {
            self.sync_updates
                .entry(sync.entity_id())
                .or_insert_with(|| cx.observe(sync, |p, _, cx| p.refresh(cx)));
        }
        let fresh = Self::snapshot(shark.read(cx), cx);
        let live: HashSet<_> = fresh.iter().map(|ws| ws.name.clone()).collect();
        let known: HashSet<_> = self.tasks.iter().map(|ws| ws.name.clone()).collect();
        let (kept, added): (Vec<_>, Vec<_>) =
            fresh.into_iter().partition(|ws| known.contains(&ws.name));
        let mut kept: HashMap<_, _> = kept.into_iter().map(|ws| (ws.name.clone(), ws)).collect();
        let mut tasks: Vec<_> = self
            .tasks
            .iter()
            .filter_map(|ws| kept.remove(&ws.name))
            .collect();
        let selected = self
            .select
            .unwrap_or_else(|| self.state.read(cx).selected_index().map(|ix| ix.row))
            .and_then(|ix| self.hits.get(ix))
            .cloned();
        let old_len = self.hits.len();
        self.hits.retain(|name| live.contains(name));
        let removed = self.hits.len() != old_len;
        let added_hits = rank(&added, &self.query);
        let changed_hits = removed || !added_hits.is_empty();
        self.hits.extend(added_hits);
        tasks.extend(added);
        if changed_hits {
            self.select = Some(
                selected
                    .and_then(|name| self.hits.iter().position(|n| n == &name))
                    .or_else(|| (!self.hits.is_empty()).then_some(0)),
            );
        }
        let minute = now_ms() / 60_000;
        if self.tasks != tasks || self.minute != minute {
            self.tasks = tasks;
            self.minute = minute;
            self.command_dirty = true;
            cx.notify();
        }
    }

    fn rank(&mut self) {
        self.command_dirty = true;
        self.hits = rank(&self.tasks, &self.query);
        self.select = Some(if self.query.trim().is_empty() {
            self.hits
                .iter()
                .position(|name| Some(name) != self.current.as_ref())
                .or_else(|| (!self.hits.is_empty()).then_some(0))
        } else {
            (!self.hits.is_empty()).then_some(0)
        });
    }

    fn command(&self, window: &Window, cx: &Context<Self>) -> Command {
        let by_name: HashMap<_, _> = self.tasks.iter().map(|ws| (ws.name.as_str(), ws)).collect();
        let now = now_ms();
        let items = self
            .hits
            .iter()
            .filter_map(|name| {
                let ws = by_name.get(name.as_str())?;
                let rows = ws.repos.clone();
                let running = ws.running;
                let attention = ws.attention.clone();
                let activity = ws.activity();
                let (time, exact) = interaction_time(ws.last_interacted_at, now);
                let time = if self.current.as_ref() == Some(name) {
                    format!("Current · {time}")
                } else {
                    time
                };
                let title = ws.title.clone();
                let label = format!(
                    "{title}, {}, {time}, {exact}, {}",
                    activity.as_deref().unwrap_or("Idle"),
                    rows.iter()
                        .map(|(name, (status, ..))| format!("{name}: {status}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                let name = name.clone();
                Some(CommandItem::new().label(label.clone()).child(move |_, cx| {
                    let muted = cx.theme().muted_foreground;
                    v_flex()
                        .id(SharedString::from(format!("workspace-result-{name}")))
                        .flex_1()
                        .min_w_0()
                        .py_1()
                        .gap_1()
                        .text_sm()
                        .text_color(cx.theme().foreground)
                        .role(gpui_kit::accesskit::Role::Group)
                        .aria_label(label.clone())
                        .tooltip({
                            let label = label.clone();
                            move |w, cx| Tooltip::new(label.clone()).build(w, cx)
                        })
                        .child(
                            h_flex()
                                .w_full()
                                .gap_2()
                                .items_center()
                                .child(
                                    div()
                                        .id("workspace-indicator")
                                        .test_support()
                                        .size(px(16.))
                                        .flex_none()
                                        .child(sidebar::task_indicator(
                                            running,
                                            &attention,
                                            muted,
                                            px(16.),
                                            cx.theme(),
                                        )),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(title.clone()),
                                )
                                .child(
                                    div()
                                        .id("interaction-time")
                                        .max_w(relative(0.55))
                                        .truncate()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(time.clone())
                                        .tooltip({
                                            let exact = exact.clone();
                                            move |w, cx| Tooltip::new(exact.clone()).build(w, cx)
                                        }),
                                ),
                        )
                        .when_some(activity.clone(), |d, activity| {
                            d.child(
                                div()
                                    .id("workspace-activity")
                                    .test_support()
                                    .ml_2()
                                    .pl(px(16.))
                                    .truncate()
                                    .text_xs()
                                    .text_color(match attention {
                                        Attn::Needs(_) => cx.theme().warning_foreground,
                                        Attn::Unread if running == 0 => {
                                            cx.theme().success_foreground
                                        }
                                        _ => muted,
                                    })
                                    .child(activity),
                            )
                        })
                        .children(rows.iter().map(|(repo, (label, icon, pr))| {
                            h_flex()
                                .ml_2()
                                .pl(px(16.))
                                .gap_2()
                                .items_center()
                                .text_xs()
                                .text_color(muted)
                                .child(
                                    div()
                                        .min_w_0()
                                        .max_w(relative(0.45))
                                        .truncate()
                                        .child(repo.clone()),
                                )
                                .child(
                                    match pr {
                                        Some(pr) => pr.icon(cx.theme()),
                                        None => Icon::new(*icon).text_color(muted),
                                    }
                                    .size_3()
                                    .flex_none(),
                                )
                                .child(div().flex_1().min_w_0().truncate().child(label.clone()))
                        }))
                        .test_support()
                }))
            })
            .collect::<Vec<_>>();
        let query = cx.entity().downgrade();
        let names = self.hits.clone();
        let shark = self.shark.clone();
        let empty = self.tasks.is_empty();
        crate::ui::command(&self.state, window)
            .items(items)
            .filterable(false)
            .bordered(false)
            .placeholder("Open task…")
            .max_h(
                (window.viewport_size().height - px(200.))
                    .max(px(48.))
                    .min(px(420.)),
            )
            .empty(move |_, _, cx| {
                div()
                    .id(if empty {
                        "workspace-picker-empty"
                    } else {
                        "workspace-picker-no-match"
                    })
                    .test_support()
                    .p_4()
                    .text_color(cx.theme().muted_foreground)
                    .child(if empty {
                        "No tasks yet"
                    } else {
                        "No matching tasks"
                    })
            })
            .footer(|_, _, cx| {
                div()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("↑↓ Navigate · ↵ Open · Esc Close")
            })
            .on_query(move |q, _, cx| {
                query
                    .update(cx, |p, cx| {
                        p.query = q.into();
                        p.rank();
                        cx.notify();
                    })
                    .ok();
            })
            .on_confirm(move |ix, w, cx| {
                let Some(name) = names.get(ix.row) else {
                    return;
                };
                w.close_dialog(cx);
                shark
                    .update(cx, |s, cx| s.open_task_named(name, w, cx))
                    .ok();
            })
    }
}

impl Render for TaskPicker {
    fn render(&mut self, w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Custom Command children are remeasured for every model update, even when unchanged.
        if self.command_dirty || self.viewport != w.viewport_size() {
            let command = self.command(w, cx);
            RenderOnce::render(command, w, cx);
            self.command_dirty = false;
            self.viewport = w.viewport_size();
        }
        if let Some(selected) = self.select.take() {
            self.state.update(cx, |s, cx| {
                s.set_selected_index(selected.map(IndexPath::new), w, cx)
            });
        }
        self.state.clone()
    }
}

impl Shark {
    pub(super) fn open_task_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.app_switcher.is_some() {
            return self.cycle_open_app(true, window, cx);
        }
        let tasks = TaskPicker::snapshot(self, cx);
        let current = (!self.home.on).then(|| self.active.clone()).flatten();
        let owner = cx.entity().downgrade();
        let picker = cx.new(|cx| TaskPicker::new(tasks, current, owner, window, cx));
        let state = picker.read(cx).state.clone();
        window.open_dialog(cx, move |d, w, cx| {
            menu_dialog(d, cx)
                .w(px(640.).min((w.viewport_size().width - px(32.)).max(px(0.))))
                .margin_top(px(96.).min(w.viewport_size().height * 0.15))
                .child(picker.clone())
        });
        state.update(cx, |s, cx| s.focus(window, cx));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn task(name: &str, repo: &str, at: Option<u64>) -> PickerTask {
        PickerTask::new(
            &Task {
                name: name.into(),
                last_interacted_at: at,
                repos: vec![crate::state::TaskRepo {
                    name: repo.into(),
                    src: PathBuf::new(),
                    base: "main".into(),
                    branch: None,
                }],
                ..Default::default()
            },
            None,
            0,
            Attn::None,
        )
    }

    #[test]
    fn activity_shows_running_counts_and_preserves_questions() {
        let mut ws = task("test", "api", None);
        assert_eq!(ws.activity(), None);
        ws.attention = Attn::Unread;
        assert_eq!(ws.activity().as_deref(), Some("Unread"));
        ws.running = 1;
        assert_eq!(ws.activity().as_deref(), Some("1 chat running"));
        ws.running = 2;
        assert_eq!(ws.activity().as_deref(), Some("2 chats running"));
        ws.attention = Attn::Needs("Question to answer".into());
        assert_eq!(
            ws.activity().as_deref(),
            Some("2 chats running · Question to answer")
        );
        ws.running = 0;
        assert_eq!(ws.activity().as_deref(), Some("Question to answer"));
    }

    #[test]
    fn relevance_recency_sidebar_and_unicode() {
        let tasks = vec![
            task("old", "api", None),
            task("api", "web", Some(1)),
            task("recent", "api", Some(3)),
            task("a-prolonged-name-i", "éK", Some(5)),
            task("tie", "api", Some(3)),
        ];
        assert_eq!(
            rank(&tasks, ""),
            ["a-prolonged-name-i", "recent", "tie", "api", "old"]
        );
        assert_eq!(
            rank(&tasks, "a"),
            ["recent", "tie", "api", "old", "a-prolonged-name-i"]
        );
        assert_eq!(
            rank(&tasks, "ap"),
            ["recent", "tie", "api", "old", "a-prolonged-name-i"]
        );
        assert_eq!(
            rank(&tasks, "api"),
            ["recent", "tie", "api", "old", "a-prolonged-name-i"]
        );
        assert_eq!(rank(&tasks, "Ék"), ["a-prolonged-name-i"]);
        assert!(rank(&tasks, "no-match").is_empty());
        assert!(
            rank(&[task("ab", "cd", None)], "bc").is_empty(),
            "never match across name/repo boundaries"
        );
    }

    #[test]
    fn renamed_agents_rank_by_title_and_keep_their_identity_at_scale() {
        for count in [100, 1000] {
            let tasks: Vec<_> = (0..count)
                .map(|i| {
                    let mut agent = task(&format!("agent-{i:06}"), "api", Some(i));
                    agent.title = "Investigate connection failures".into();
                    agent
                })
                .collect();
            let start = std::time::Instant::now();
            for _ in 0..20 {
                let hits = rank(&tasks, "connection failures");
                assert_eq!(hits.len(), count as usize);
                assert_eq!(hits[0], format!("agent-{:06}", count - 1));
            }
            eprintln!(
                "rank {count} renamed tasks: {:?} per query",
                start.elapsed() / 20
            );
            assert_eq!(rank(&tasks, "agent-000000")[0], "agent-000000");
        }
    }

    #[test]
    fn spaced_queries_match_words_in_task_and_repo_names() {
        let tasks = vec![
            task("eng-130-rate-limits", "web", Some(3)),
            task("rate_limits", "api", Some(2)),
            task("billing", "rate.limits", Some(1)),
            task("rate", "limits", Some(4)),
        ];
        for query in [
            "rate limits",
            "  RATE   LIMITS ",
            "rate-limits",
            "rate_limits",
        ] {
            let hits = rank(&tasks, query);
            assert_eq!(hits.len(), 3, "{query}: {hits:?}");
            assert!(hits.iter().any(|name| name == "eng-130-rate-limits"));
            assert!(
                !hits.iter().any(|name| name == "rate"),
                "never combine workspace and repo names"
            );
        }
    }

    #[test]
    fn timestamps_survive_serde_and_unknown_stays_unknown() {
        let old = r#"{"name":"old","branch":"old","repos":[]}"#;
        let mut ws: Task = serde_json::from_str(old).unwrap();
        assert_eq!(ws.last_interacted_at, None);
        ws.last_interacted_at = Some(1_793_117_632_123);
        let read: Task = serde_json::from_str(&serde_json::to_string(&ws).unwrap()).unwrap();
        assert_eq!(read.last_interacted_at, ws.last_interacted_at);
        assert_eq!(interaction_time(None, 0).0, "No recorded interaction");
        assert_eq!(interaction_time(Some(60_000), 0).0, "Just now");
        assert_eq!(interaction_time(Some(0), 720_000).0, "12m ago");
        assert!(
            interaction_time(Some(0), 0)
                .1
                .contains("1970-01-01 00:00:00")
        );
    }

    #[test]
    fn explicit_pr_lifecycle_and_unavailable_states() {
        let ws = Task {
            name: "test".into(),
            repos: vec![crate::state::TaskRepo {
                name: "api".into(),
                src: PathBuf::new(),
                base: "main".into(),
                branch: None,
            }],
            ..Default::default()
        };
        let mut repo = SyncRepo::fresh(&ws.repos[0], &ws);
        assert_eq!(status(None).0, "Not checked");
        assert_eq!(status(Some(&repo)).0, "Not checked");
        repo.pr = Some(Err("offline".into()));
        assert_eq!(status(Some(&repo)).0, "Status unavailable");
        repo.pr = Some(Ok(None));
        assert_eq!(status(Some(&repo)).0, "No PR");
        for (state, draft, label) in [
            ("OPEN", false, "Open"),
            ("OPEN", true, "Draft"),
            ("MERGED", true, "Merged"),
            ("CLOSED", true, "Closed"),
        ] {
            repo.pr = Some(Ok(crate::sync::parse_prs(&format!(
                r#"[{{"number":142,"state":"{state}","isDraft":{draft},"statusCheckRollup":[{{"__typename":"CheckRun","status":"COMPLETED","conclusion":"FAILURE"}}]}}]"#
            ))));
            assert_eq!(status(Some(&repo)).0, format!("{label} #142"));
        }
    }
}
