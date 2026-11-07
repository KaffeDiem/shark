//! Linear issues on an agent: attaching one, its details, and starting an agent from one. An
//! attachment is reference material: it starts no turn and waits for nothing, and chats hear of
//! it through `Task::context`. Nothing here writes to Linear.
use super::attach::ChipCard;
use super::*;
use crate::linear::{self, Credential, Error, Hit, Http, IssueRef, Lists, Page, Target};
use crate::sync::ago;
use crate::ui::menu_dialog;
use crate::usage::now;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::input::{Enter, Textarea, TextareaState};
use gpui_kit::component::skeleton::Skeleton;
use gpui_kit::component::text::TextViewState;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const DEBOUNCE: Duration = Duration::from_millis(300);
const MIN_QUERY: usize = 2;
const MINUTE: Duration = Duration::from_secs(60);
/// Under `searchIssues`' 30 a minute, which other pickers and clients share.
const SEARCHES_A_MINUTE: usize = 25;
/// Command's row padding brings this to 36px.
const ROW_H: f32 = 25.;

/// A Linear state's icon and color. `state.type`, so a custom state like "Merged" gets one too;
/// an unknown type or a snapshot from before it was kept reads as unstarted.
fn status_icon(kind: &str, theme: &Theme) -> Icon {
    let (icon, color) = match kind {
        "triage" => (IconName::Inbox, theme.warning_foreground),
        "backlog" => (IconName::CircleDashed, theme.muted_foreground),
        "started" => (IconName::CircleDot, theme.warning_foreground),
        "completed" => (IconName::CircleCheck, theme.success_foreground),
        "canceled" | "duplicate" => (IconName::CircleX, theme.muted_foreground),
        _ => (IconName::Circle, theme.muted_foreground),
    };
    Icon::new(icon).text_color(color)
}

/// The priority's name and icon, and whether it's urgent.
fn priority(p: u8) -> Option<(&'static str, IconName, bool)> {
    match p {
        1 => Some(("Urgent", IconName::TriangleAlert, true)),
        2 => Some(("High", IconName::SignalHigh, false)),
        3 => Some(("Medium", IconName::SignalMedium, false)),
        4 => Some(("Low", IconName::SignalLow, false)),
        _ => None,
    }
}

/// "KM" for Kasper Munch.
fn initials(name: &str) -> String {
    (name.split_whitespace().take(2))
        .filter_map(|w| w.chars().next())
        .flat_map(char::to_uppercase)
        .collect()
}

/// "3d", for a narrow column.
fn age(iso: &str) -> String {
    (chrono::DateTime::parse_from_rfc3339(iso).ok())
        .map(|t| {
            (chrono::Utc::now() - t.to_utc())
                .to_std()
                .unwrap_or_default()
        })
        .map_or(String::new(), |d| ago(d).replace(" ago", ""))
}

/// Where `query`, already lower-cased, sits in `title`.
fn title_match(title: &str, query: &str) -> Option<std::ops::Range<usize>> {
    let at = title
        .to_lowercase()
        .find(query)
        .filter(|_| !query.is_empty())?;
    let end = at + query.len();
    (title.is_char_boundary(at) && title.is_char_boundary(end)).then_some(at..end)
}

fn matches(h: &Hit, query: &str) -> bool {
    let has = |s: &str| s.to_lowercase().contains(query);
    has(&h.identifier) || has(&h.title)
}

/// An issue's row: status icon, identifier and title, then muted status, priority, owner and age,
/// and a check at the end when it's attached or marked.
fn hit_item(h: Hit, query: &str, attached: bool, marked: bool) -> CommandItem {
    let level = priority(h.priority);
    let owner = h.assignee.as_deref().unwrap_or("Unassigned");
    let label = format!(
        "{} {}, {}, {}, {owner}{}",
        h.identifier,
        h.title,
        h.status,
        level.map_or("No priority", |l| l.0),
        if attached { ", attached" } else { "" },
    );
    let matched = title_match(&h.title, query);
    CommandItem::new().label(label.clone()).child(move |_, cx| {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let cell = |w: f32| div().w(px(w)).flex_none().text_xs().text_color(muted);
        let bold = HighlightStyle {
            font_weight: Some(FontWeight::SEMIBOLD),
            ..Default::default()
        };
        let tip = label.clone();
        h_flex()
            .id(SharedString::from(format!("issue-row-{}", h.identifier)))
            .test_support()
            .flex_1()
            .min_w_0()
            .h(px(ROW_H))
            .gap_2()
            .aria_label(label.clone())
            .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
            .child(
                status_icon(&h.state_type, theme)
                    .with_size(px(14.))
                    .flex_none(),
            )
            .child(cell(64.).truncate().child(h.identifier.clone()))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.foreground)
                    .child(
                        StyledText::new(h.title.clone())
                            .with_highlights(matched.clone().map(|r| (r, bold))),
                    ),
            )
            .child(cell(76.).truncate().text_right().child(h.status.clone()))
            .child(div().size(px(14.)).flex_none().when_some(level, |d, l| {
                let color = if l.2 { theme.danger_foreground } else { muted };
                d.child(Icon::new(l.1).with_size(px(14.)).text_color(color))
            }))
            .child(
                h_flex()
                    .size(px(20.))
                    .flex_none()
                    .justify_center()
                    .when_some(h.assignee.as_deref(), |d, name| {
                        d.rounded_full()
                            .bg(theme.muted)
                            .text_xs()
                            .text_color(muted)
                            .child(initials(name))
                    }),
            )
            .child(cell(26.).text_right().child(age(&h.updated_at)))
            .child(
                div()
                    .size(px(14.))
                    .flex_none()
                    .when(attached || marked, |d| {
                        let (icon, color) = match marked {
                            true => (IconName::SquareCheck, theme.foreground),
                            false => (IconName::Check, muted),
                        };
                        d.child(
                            div()
                                .id("issue-check")
                                .test_support()
                                .child(Icon::new(icon).with_size(px(14.)).text_color(color)),
                        )
                    }),
            )
    })
}

/// A row that isn't an issue: a spinner without `icon`, and what it does at the end.
fn line_item(
    id: &'static str,
    icon: Option<IconName>,
    text: String,
    end: Option<&'static str>,
    danger: bool,
) -> CommandItem {
    CommandItem::new().label(text.clone()).child(move |_, cx| {
        let tip = text.clone();
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        h_flex()
            .id(id)
            .test_support()
            .flex_1()
            .min_w_0()
            .h(px(ROW_H))
            .gap_2()
            .when(danger, |d| d.text_color(theme.danger_foreground))
            .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
            .map(|d| match icon {
                Some(icon) => d.child(Icon::new(icon).with_size(px(14.))),
                None => d.child(Spinner::new().with_size(px(14.)).color(muted)),
            })
            .child(div().flex_1().min_w_0().truncate().child(text.clone()))
            .when_some(end, |d, end| {
                d.child(div().flex_none().text_xs().text_color(muted).child(end))
            })
    })
}

/// Why the picker can't ask Linear anything.
#[derive(Clone, PartialEq)]
enum Blocked {
    NotConnected,
    /// What `linear_needs_attention` says.
    Attention(String),
}

impl Blocked {
    /// What `enter` does about it, which is to go to Settings.
    fn action(&self) -> &'static str {
        match self {
            Self::NotConnected => "Open Settings",
            Self::Attention(_) => "Reconnect",
        }
    }
}

/// The row in place of the issues. Not `line_item`: the reason wraps, since it's all there is.
fn blocked_item(blocked: &Blocked) -> CommandItem {
    let action = blocked.action();
    let (attention, text) = match blocked {
        Blocked::NotConnected => (false, "Linear isn't connected".to_string()),
        Blocked::Attention(why) => (true, why.clone()),
    };
    CommandItem::new().label(text.clone()).child(move |_, cx| {
        let theme = cx.theme();
        let (icon, color) = match attention {
            true => (IconName::TriangleAlert, theme.warning_foreground),
            false => (IconName::CircleDashed, theme.muted_foreground),
        };
        h_flex()
            .id("issue-settings")
            .test_support()
            .flex_1()
            .min_w_0()
            .min_h(px(ROW_H))
            .gap_2()
            .aria_label(text.clone())
            .child(Icon::new(icon).with_size(px(14.)).text_color(color))
            .child(div().flex_1().min_w_0().child(text.clone()))
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(action),
            )
    })
}

/// The first load's placeholder row.
fn skeleton_item() -> CommandItem {
    CommandItem::new().disabled(true).child(|_, _| {
        h_flex()
            .id("issue-skeleton")
            .test_support()
            .flex_1()
            .h(px(ROW_H))
            .gap_2()
            .child(Skeleton::new().size(px(14.)).rounded_full())
            .child(Skeleton::new().w(px(56.)).h(px(10.)))
            .child(Skeleton::new().flex_1().h(px(10.)))
    })
}

/// What a row of the picker does.
#[derive(Clone)]
enum Row {
    Settings(Blocked),
    /// Linear's rate limit counting down.
    Wait,
    /// The last failure, and Retry.
    Retry,
    Skeleton,
    /// What the query names, until its answer replaces this.
    Lookup(Target),
    Hit(Hit),
    More,
}

impl Row {
    /// Which row this is whatever its number, so the highlight can follow it.
    fn key(&self) -> Option<&str> {
        match self {
            Self::Settings(_) => Some("settings"),
            Self::Retry => Some("retry"),
            Self::More => Some("more"),
            Self::Lookup(t) => Some(&t.id),
            Self::Hit(h) => Some(&h.identifier),
            Self::Wait | Self::Skeleton => None,
        }
    }
}

/// What Retry runs.
enum Job {
    Search,
    More,
    Fetch(Vec<Target>),
}

/// Attach Linear issue and Start from Linear issue. Rows answer the query as it's typed: first
/// from what's loaded, then from one debounced search. Nothing outlives the picker but a read
/// already on the wire: every timer is a task it owns, so closing it stops them.
pub(super) struct IssuePicker {
    state: Entity<CommandState>,
    /// The agent to attach to; None starts a new one.
    agent: Option<String>,
    /// Why nothing can be asked of Linear; the row to Settings replaces the others.
    blocked: Option<Blocked>,
    /// Lists, cached results and marks belong to this connection.
    connection: (Option<crate::linear::Connection>, Option<u64>),
    query: String,
    lists: Option<Lists>,
    /// Every answer by query, for the picker's lifetime: asking again sends nothing.
    cache: HashMap<String, Page>,
    /// The `cache` entry on screen until the query's own answer lands; none shows `lists`.
    shown: String,
    /// The highlighted row's key, once it's been moved. Command selects by row number.
    cursor: Option<String>,
    marks: Vec<Target>,
    /// Goes up with every query change; an answer asked at a lower one is stale.
    seq: u64,
    /// A search is on the wire. Up only in `launch`, down when its task ends, whatever the
    /// outcome: the task is never replaced while this is up, and curl's own timeout ends it.
    flying: bool,
    /// The debounce fired during a flight: send the latest query when it lands.
    pending: bool,
    /// When the searches of the last minute went out.
    // ponytail: per picker. A reopened one starts counting again; Linear's 429 covers that.
    sends: VecDeque<Instant>,
    /// Seconds until Linear takes requests again.
    wait: Option<u64>,
    fetching: bool,
    /// The last failure; the query and the rows stay.
    failure: Option<(String, Job)>,
    shark: WeakEntity<Shark>,
    _debounce: gpui_kit::Task<()>,
    _flight: gpui_kit::Task<()>,
    _wait: gpui_kit::Task<()>,
    /// Closing the dialog drops this: what it would have attached is abandoned.
    _fetch: gpui_kit::Task<()>,
    _connection: Subscription,
    /// Settings over this picker, until its view is dropped with the dialog.
    settings: Option<WeakFocusHandle>,
}

impl IssuePicker {
    fn key(&self) -> String {
        self.query.trim().to_lowercase()
    }

    /// The query is one a search answers, and the answer isn't here yet.
    fn unanswered(&self) -> bool {
        let key = self.key();
        key.chars().count() >= MIN_QUERY && !self.cache.contains_key(&key)
    }

    fn searching(&self) -> bool {
        let idle = self.blocked.is_some() || self.wait.is_some() || self.failure.is_some();
        self.flying || self.fetching || (self.unanswered() && !idle)
    }

    /// The rows by group, none of them empty.
    fn groups(&self) -> Vec<(Option<&'static str>, Vec<Row>)> {
        if let Some(blocked) = &self.blocked {
            return vec![(None, vec![Row::Settings(blocked.clone())])];
        }
        let key = self.key();
        let answer = self.cache.get(&key);
        let lookup = linear::parse_issue(&self.query).filter(|_| answer.is_none());
        // Load more's failure takes its place, where the list has scrolled to.
        let more_failed = matches!(self.failure, Some((_, Job::More)));
        let top = [
            self.wait.map(|_| Row::Wait),
            self.failure
                .as_ref()
                .filter(|_| !more_failed)
                .map(|_| Row::Retry),
            lookup.map(Row::Lookup),
        ];
        let mut groups = vec![(None, top.into_iter().flatten().collect::<Vec<_>>())];
        let shown = self.cache.get(&self.shown);
        let sources = match (&self.lists, shown) {
            (_, Some(page)) => vec![(None, &page.hits)],
            (Some(l), None) => vec![
                (Some("Assigned to me"), &l.assigned),
                (Some("Recent"), &l.recent),
            ],
            (None, None) => vec![],
        };
        if let Some(page) = answer {
            let hits = page.hits.iter().cloned().map(Row::Hit);
            let more = page.next.as_ref().map(|_| match more_failed {
                true => Row::Retry,
                false => Row::More,
            });
            groups.push((None, hits.chain(more).collect()));
        } else if sources.is_empty() {
            if self.wait.is_none() && self.failure.is_none() {
                groups.push((None, vec![Row::Skeleton; 6]));
            }
        } else {
            // Nothing loaded matches, so what's there stays until the answer lands.
            let hit = sources
                .iter()
                .any(|(_, h)| h.iter().any(|h| matches(h, &key)));
            let keep = !hit && self.unanswered();
            groups.extend(sources.into_iter().map(|(heading, hits)| {
                let hits = hits.iter().filter(|h| keep || matches(h, &key));
                (heading, hits.cloned().map(Row::Hit).collect())
            }));
        }
        groups.retain(|(_, rows)| !rows.is_empty());
        groups
    }

    fn row_at(&self, at: IndexPath) -> Option<Row> {
        self.groups().get(at.section)?.1.get(at.row).cloned()
    }

    /// The agent's issues as (UUID, identifier).
    fn attached(&self, cx: &App) -> Vec<(String, String)> {
        let shark = self.shark.upgrade();
        let ws = (self.agent.as_ref())
            .zip(shark.as_ref())
            .and_then(|(n, s)| s.read(cx).task(n));
        ws.map_or(vec![], |ws| {
            (ws.linear.ordered())
                .map(|r| (r.issue.clone(), issue_label(ws, r)))
                .collect()
        })
    }

    fn item(&self, row: &Row, attached: &[(String, String)]) -> CommandItem {
        let lookup_attached = |t: &Target| t.org.is_none() && attached.iter().any(|a| a.1 == t.id);
        let marked = |id: &str| self.marks.iter().any(|t| t.id == id);
        match row {
            Row::Settings(blocked) => blocked_item(blocked),
            Row::Wait => {
                let left = self.wait.unwrap_or_default();
                let text = format!("Linear is rate limiting searches. Resuming in {left}s");
                line_item("issue-wait", Some(IconName::Clock), text, None, false).disabled(true)
            }
            Row::Retry => {
                let text = self.failure.as_ref().map_or(String::new(), |f| f.0.clone());
                let icon = Some(IconName::TriangleAlert);
                line_item("issue-notice", icon, text, Some("Retry"), true)
            }
            Row::Skeleton => skeleton_item(),
            Row::Lookup(t) => {
                let end = lookup_attached(t).then_some("Attached");
                let end = end.or(marked(&t.id).then_some("Marked"));
                let text = format!("Look up {}", t.id);
                line_item("issue-lookup", Some(IconName::Search), text, end, false)
            }
            Row::Hit(h) => {
                let done = attached.iter().any(|a| a.0 == h.id);
                hit_item(h.clone(), &self.key(), done, marked(&h.identifier))
            }
            Row::More => {
                let icon = Some(IconName::ChevronDown);
                line_item("issue-more", icon, "Load more".into(), None, false)
            }
        }
    }

    fn typed(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.query = query.to_string();
        self.seq += 1;
        self.cursor = None;
        self.pending = false;
        self.failure = None;
        let key = self.key();
        if self.cache.contains_key(&key) {
            self.shown = key;
            self._debounce = gpui_kit::Task::ready(());
        } else {
            if key.chars().count() < MIN_QUERY {
                self.shown.clear();
            }
            self.debounce(DEBOUNCE, window, cx);
        }
        cx.notify();
    }

    fn debounce(&mut self, delay: Duration, window: &mut Window, cx: &mut Context<Self>) {
        self._debounce = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update_in(cx, |p, window, cx| p.send(window, cx)).ok();
        });
    }

    /// Asks Linear for what the rows lack, if it can be asked now; otherwise it's asked once it
    /// can, with the query of then.
    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.blocked.is_some() {
            return;
        }
        if self.flying || self.wait.is_some() {
            self.pending = true;
            return;
        }
        if self.unanswered() {
            let now = cx.background_executor().now();
            self.sends.retain(|t| now.duration_since(*t) < MINUTE);
            match self
                .sends
                .front()
                .filter(|_| self.sends.len() >= SEARCHES_A_MINUTE)
            {
                Some(first) => {
                    let free = MINUTE - now.duration_since(*first);
                    self.debounce(free, window, cx)
                }
                None => self.search(None, window, cx),
            }
        } else if self.lists.is_none() && self.key().chars().count() < MIN_QUERY {
            self.launch(
                linear::lists,
                |p, result, window, cx| match result {
                    Ok(lists) => p.lists = Some(lists),
                    Err(e) => p.fail(e, Job::Search, true, window, cx),
                },
                window,
                cx,
            );
        }
    }

    /// The query's answer, or with `after` its next page. Identifiers and URLs resolve here too.
    fn search(&mut self, after: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let (seq, key, term) = (self.seq, self.key(), self.query.trim().to_string());
        let more = after.is_some();
        let now = cx.background_executor().now();
        self.sends.push_back(now);
        self.launch(
            move |http, c| match &after {
                Some(_) => linear::search(http, c, &term, after.as_deref()),
                None => linear::find(http, c, &term),
            },
            move |p, result, window, cx| {
                let fresh = seq == p.seq;
                match result {
                    Ok(page) => {
                        let known = p.cache.entry(key.clone()).or_default();
                        if more && fresh {
                            p.cursor = page.hits.first().map(|h| h.identifier.clone());
                        }
                        known.next = page.next;
                        known.hits.extend(page.hits);
                        if fresh {
                            p.shown = key;
                        }
                    }
                    Err(e) => {
                        let job = if more { Job::More } else { Job::Search };
                        p.fail(e, job, fresh, window, cx)
                    }
                }
            },
            window,
            cx,
        );
    }

    /// The one read of the search channel; `send` and `more` check it's free.
    fn launch<T: Send + 'static>(
        &mut self,
        call: impl FnOnce(&Http, &Credential) -> Result<T, Error> + Send + 'static,
        done: impl FnOnce(&mut Self, Result<T, Error>, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(task) = self.shark.update(cx, |s, cx| s.linear_request(call, cx)) else {
            return;
        };
        (self.flying, self.failure) = (true, None);
        self._flight = cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |p, window, cx| {
                p.flying = false;
                done(p, result, window, cx);
                if std::mem::take(&mut p.pending) {
                    p.send(window, cx);
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    /// A stale failure says nothing about the rows, unless Linear refused the connection itself.
    fn fail(
        &mut self,
        e: Error,
        job: Job,
        fresh: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match e {
            Error::RateLimited { retry_at } => self.pause(retry_at, window, cx),
            // `linear_rejected` came first, so `connection_changed` has said it.
            Error::Unauthorized => {}
            e if fresh => {
                if matches!(job, Job::More) {
                    self.cursor = Some("retry".into());
                }
                self.failure = Some((e.to_string(), job));
            }
            _ => {}
        }
    }

    /// Stops sending until `retry_at`, then asks for the latest query. Counts in timer ticks, so
    /// a test's clock drives it.
    fn pause(&mut self, retry_at: u64, window: &mut Window, cx: &mut Context<Self>) {
        let wait = retry_at.saturating_sub(now() as u64).max(1);
        self.wait = Some(wait);
        self._wait = cx.spawn_in(window, async move |this, cx| {
            for left in (0..wait).rev() {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                this.update_in(cx, |p, window, cx| {
                    p.wait = Some(left).filter(|left| *left > 0);
                    if left == 0 {
                        p.send(window, cx);
                    }
                    cx.notify();
                })
                .ok();
            }
        });
    }

    /// Shark changed, perhaps through Settings over this picker. Once Linear can be asked,
    /// Settings gives way if it's still up, and the query waiting here goes out.
    fn connection_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shark) = self.shark.upgrade() else {
            return;
        };
        let s = shark.read(cx);
        let now = s.linear_blocked();
        let connection = (s.state.linear.clone(), s.linear_connection_epoch());
        let changed = connection != self.connection;
        if now == self.blocked && !changed {
            return;
        }
        if changed {
            self.connection = connection;
            self.lists = None;
            self.cache.clear();
            self.shown.clear();
            self.marks.clear();
            self.cursor = None;
            self.failure = None;
            self.wait = None;
            self.pending = false;
            self.flying = false;
            self.fetching = false;
            self._debounce = gpui_kit::Task::ready(());
            self._flight = gpui_kit::Task::ready(());
            self._wait = gpui_kit::Task::ready(());
            self._fetch = gpui_kit::Task::ready(());
        }
        let unblocked = std::mem::replace(&mut self.blocked, now).is_some();
        if (unblocked || changed) && self.blocked.is_none() && self.connection.1.is_some() {
            if (self.settings.take()).is_some_and(|f| f.upgrade().is_some()) {
                window.close_dialog(cx);
            }
            (self.cursor, self.failure) = (None, None);
            self.send(window, cx);
        }
        cx.notify();
    }

    fn more(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let after = self.cache.get(&self.key()).and_then(|p| p.next.clone());
        if !self.flying && self.wait.is_none() && after.is_some() {
            self.search(after, window, cx);
        }
    }

    fn retry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.failure.take().map(|f| f.1) {
            Some(Job::Search) => self.send(window, cx),
            Some(Job::More) => self.more(window, cx),
            Some(Job::Fetch(targets)) => self.fetch(targets, window, cx),
            None => {}
        }
        cx.notify();
    }

    fn pick(&mut self, at: IndexPath, window: &mut Window, cx: &mut Context<Self>) {
        let attached = self.attached(cx);
        let (target, done) = match self.row_at(at) {
            // Over the picker, which is there as it was when Settings closes.
            Some(Row::Settings(_)) => {
                let shark = self.shark.clone();
                shark
                    .update(cx, |s, cx| {
                        crate::settings::open_linear(&s.state, window, cx)
                    })
                    .ok();
                self.settings = window.focused(cx).map(|f| f.downgrade());
                return;
            }
            Some(Row::Retry) => return self.retry(window, cx),
            Some(Row::More) => return self.more(window, cx),
            Some(Row::Lookup(t)) => {
                let done = t.org.is_none() && attached.iter().any(|a| a.1 == t.id);
                (t, done)
            }
            Some(Row::Hit(h)) => {
                let done = attached.iter().any(|a| a.0 == h.id);
                let (id, org) = (h.identifier, None);
                (Target { id, org }, done)
            }
            _ => return,
        };
        // Marks win, as on the Attach button: what the footer counts is what gets attached.
        // ponytail: `Command` reports a click like `enter`, so clicking an unmarked row while
        // marks exist attaches the marks without it; tell them apart to take the clicked row too.
        match (self.marks.is_empty(), done) {
            (false, _) => self.fetch(self.marks.clone(), window, cx),
            (true, true) => {
                window.push_notification(format!("{} is already attached", target.id), cx)
            }
            (true, false) => self.fetch(vec![target], window, cx),
        }
    }

    /// ⌘↵: marks the highlighted row, or takes the mark away.
    fn mark(&mut self, cx: &mut Context<Self>) {
        let at = self.state.read(cx).selected_index();
        let attached = self.attached(cx);
        let target = match at.and_then(|at| self.row_at(at)) {
            Some(Row::Hit(h)) if !attached.iter().any(|a| a.0 == h.id) => Target {
                id: h.identifier,
                org: None,
            },
            Some(Row::Lookup(t)) => t,
            _ => return,
        };
        if self.agent.is_none() {
            return;
        }
        let before = self.marks.len();
        self.marks.retain(|t| t.id != target.id);
        if self.marks.len() == before {
            self.marks.push(target);
        }
        cx.notify();
    }

    /// All of `targets` or none, in one read, then done with the picker. Doesn't wait for a
    /// search: it has its own slot, one at a time.
    fn fetch(&mut self, targets: Vec<Target>, window: &mut Window, cx: &mut Context<Self>) {
        if self.wait.is_some() {
            let msg = "Linear is rate limiting requests. Try again when the countdown ends.";
            return window.push_notification(msg, cx);
        }
        if self.fetching {
            return;
        }
        let asked = targets.clone();
        let call = move |http: &Http, c: &Credential| {
            let get = |t: &Target| linear::get_issue(http, c, t);
            asked.iter().map(get).collect::<Result<Vec<_>, _>>()
        };
        let Ok(task) = self.shark.update(cx, |s, cx| s.linear_request(call, cx)) else {
            return;
        };
        (self.fetching, self.failure) = (true, None);
        self._fetch = cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |p, window, cx| {
                p.fetching = false;
                match result {
                    Ok(snapshots) => p.finish(snapshots, window, cx),
                    Err(e) => p.fail(e, Job::Fetch(targets), true, window, cx),
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    fn finish(&mut self, snapshots: Vec<Snapshot>, window: &mut Window, cx: &mut Context<Self>) {
        window.close_dialog(cx);
        let agent = self.agent.clone();
        (self.shark.update(cx, |s, cx| match agent {
            Some(name) => s.attach_issues(name, snapshots, window, cx),
            // Nothing can be marked here, so it's the one.
            None => (snapshots.into_iter()).for_each(|one| s.preview_issue(one, window, cx)),
        }))
        .ok();
    }
}

impl Render for IssuePicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let loading = self.searching();
        if self.state.read(cx).is_loading() != loading {
            self.state
                .update(cx, |s, cx| s.set_loading(loading, window, cx));
        }
        let groups = self.groups();
        let attached = self.attached(cx);
        let muted = cx.theme().muted_foreground;
        let query = self.query.trim().to_string();
        let marked = self.marks.len();
        let (nav, close) = (("up down", "Navigate"), ("escape", "Close"));
        let hints = match (&self.blocked, &self.agent, marked) {
            (Some(b), ..) => vec![("enter", b.action()), close],
            (_, None, _) => vec![nav, ("enter", "Start"), close],
            (_, _, 0) => vec![nav, ("enter", "Attach"), ("cmd-enter", "Mark"), close],
            _ => vec![
                nav,
                ("enter", "Attach marked"),
                ("cmd-enter", "Mark"),
                close,
            ],
        };
        let marked = Some(marked).filter(|n| *n > 0 && self.blocked.is_none());
        let (q, select, pick, attach) = (
            cx.entity().downgrade(),
            cx.entity().downgrade(),
            cx.entity().downgrade(),
            cx.entity().downgrade(),
        );
        let command = (groups.iter())
            .fold(
                crate::ui::command(&self.state, window),
                |c, (heading, rows)| {
                    let items = rows.iter().map(|r| self.item(r, &attached));
                    let group = CommandGroup::new().items(items);
                    c.group(match heading {
                        Some(h) => group.label(*h),
                        None => group,
                    })
                },
            )
            .bordered(false)
            .filterable(false)
            .max_h(px(380.))
            .placeholder("Search issues, or paste a Linear URL or identifier…")
            .empty(move |_, _, _| {
                let text = match query.is_empty() {
                    true => "No issues assigned to you or recently updated".to_string(),
                    false => format!("No issues match “{query}”"),
                };
                div()
                    .id("issue-empty")
                    .test_support()
                    .p_3()
                    .text_sm()
                    .text_color(muted)
                    .child(text)
            })
            .footer(move |_, _, _| {
                let attach = attach.clone();
                let action = marked.map(|n| {
                    h_flex()
                        .gap_2()
                        .child(format!("{n} marked"))
                        .child(
                            Button::new("issue-attach-marked")
                                .primary()
                                .xsmall()
                                .label("Attach")
                                .on_click(move |_, window, cx| {
                                    attach
                                        .update(cx, |p, cx| p.fetch(p.marks.clone(), window, cx))
                                        .ok();
                                }),
                        )
                        .into_any_element()
                });
                crate::ui::key_footer(&hints, action).text_color(muted)
            })
            .on_query(move |query, window, cx| {
                q.update(cx, |p, cx| p.typed(query, window, cx)).ok();
            })
            .on_select(move |at, _, cx| {
                select
                    .update(cx, |p, _| {
                        p.cursor = p.row_at(at).and_then(|r| r.key().map(str::to_string))
                    })
                    .ok();
            })
            .on_confirm(move |at, window, cx| {
                pick.update(cx, |p, cx| p.pick(at, window, cx)).ok();
            });
        // Install the new rows, then put the highlight back on the row it was on.
        let command = RenderOnce::render(command, window, cx).into_any_element();
        if let Some(cursor) = self.cursor.as_deref() {
            let rows = || {
                (groups.iter().enumerate()).flat_map(|(section, (_, rows))| {
                    let at = move |row| IndexPath::new(row).section(section);
                    (rows.iter().enumerate()).map(move |(row, r)| (at(row), r))
                })
            };
            let row = rows().find(|(_, r)| r.key() == Some(cursor));
            let at = row.or(rows().find(|(_, r)| r.key().is_some())).map(|r| r.0);
            self.state.update(cx, |s, cx| {
                if s.selected_index() != at {
                    s.set_selected_index(at, window, cx);
                }
            });
        }
        div()
            .on_action(cx.listener(|p, e: &Enter, _, cx| match e.secondary {
                true => p.mark(cx),
                false => cx.propagate(),
            }))
            .child(command)
    }
}

/// The first request of an agent started from an issue: to plan, which is all it can do so far.
fn planning_request(s: &Snapshot) -> String {
    format!(
        "Read the attached Linear issue {} and draft a plan for it. List anything unclear before going further.",
        s.identifier
    )
}

fn synced(s: &Snapshot) -> String {
    match (now() as u64).saturating_sub(s.retrieved_at) {
        0..60 => "Synced just now".into(),
        secs => format!("Synced {}", ago(Duration::from_secs(secs))),
    }
}

/// An issue as details and previews show it.
fn issue_summary(
    s: &Snapshot,
    description: &Entity<TextViewState>,
    connected: bool,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let owner = s.assignee.as_deref().unwrap_or("Unassigned");
    let facts = [
        Some(s.status.as_str()),
        Some(owner),
        Some(s.team.as_str()),
        s.project.as_deref(),
    ];
    let facts: Vec<&str> = facts.into_iter().flatten().collect();
    let exact = format!("Snapshot from {}", s.refreshed());
    v_flex()
        .id("issue-summary")
        .px_3()
        .py_2()
        .gap_1p5()
        .child(
            div()
                .text_base()
                .font_weight(FontWeight::MEDIUM)
                .child(format!("{} {}", s.identifier, s.title)),
        )
        .child(
            h_flex()
                .id("issue-facts")
                .test_support()
                .gap_1p5()
                .text_xs()
                .text_color(theme.muted_foreground)
                .tooltip(move |w, cx| Tooltip::new(exact.clone()).build(w, cx))
                .child(
                    status_icon(&s.state_type, theme)
                        .with_size(px(12.))
                        .flex_none(),
                )
                .child(format!("{} · {}", facts.join(" · "), synced(s))),
        )
        .when(!connected, |d| {
            d.child(
                div().text_xs().text_color(theme.muted_foreground).child(
                    "Disconnected: from another Linear organization, or Linear isn't connected",
                ),
            )
        })
        .when(!s.description.is_empty(), |d| {
            d.child(
                div()
                    .id("issue-description")
                    .test_support()
                    .group("issue-description")
                    .max_h(px(240.))
                    .overflow_y_scroll()
                    .text_size(px(14.))
                    .line_height(relative(1.5))
                    .child(crate::ui::markdown(
                        description,
                        "issue-description",
                        vec![],
                    )),
            )
        })
        .test_support()
        .into_any_element()
}

fn issue_label(ws: &Task, r: &IssueRef) -> String {
    r.of(&ws.issues)
        .map_or("Linear issue".into(), |s| s.identifier.clone())
}

/// What the details dialog can do, top to bottom.
#[derive(Clone, Copy)]
enum Action {
    Open,
    Refresh,
    Primary,
    Remove,
}

/// An attached issue and what can be done about it. Stays open through Refresh and Make primary,
/// so what they did shows in place.
struct IssueDetails {
    name: String,
    r: IssueRef,
    state: Entity<CommandState>,
    description: Entity<TextViewState>,
    /// What `description` holds.
    text: String,
    refreshing: bool,
    shark: WeakEntity<Shark>,
    _shark: Subscription,
}

impl IssueDetails {
    /// `shark`'s view of the issue: its snapshot and whether it's primary.
    fn issue(&self, cx: &App) -> Option<(Option<Snapshot>, bool, String)> {
        let shark = self.shark.upgrade()?;
        let ws = shark.read(cx).task(&self.name)?;
        let primary = ws.linear.primary() == Some(&self.r);
        Some((
            self.r.of(&ws.issues).cloned(),
            primary,
            issue_label(ws, &self.r),
        ))
    }

    fn actions(snapshot: bool, primary: bool) -> Vec<Action> {
        let open = snapshot.then_some(Action::Open);
        let make = (!primary).then_some(Action::Primary);
        (open.into_iter())
            .chain([Action::Refresh])
            .chain(make)
            .chain([Action::Remove])
            .collect()
    }

    fn run(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((snapshot, primary, id)) = self.issue(cx) else {
            return;
        };
        let (name, r) = (self.name.clone(), self.r.clone());
        match Self::actions(snapshot.is_some(), primary).get(row) {
            Some(Action::Open) => {
                if let Some(s) = snapshot {
                    cx.open_url(&s.url);
                }
            }
            Some(Action::Refresh) if !self.refreshing => {
                let refresh =
                    (self.shark).update(cx, |s, cx| s.refresh_attached(&name, &r, window, cx));
                let Ok(refresh) = refresh else { return };
                self.refreshing = true;
                // Detached: closing the dialog doesn't drop what Linear answers.
                cx.spawn(async move |this, cx| {
                    refresh.await;
                    this.update(cx, |d, cx| {
                        d.refreshing = false;
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            Some(Action::Primary) => {
                (self.shark.update(cx, |s, cx| s.make_primary(&name, &r, cx))).ok();
                let done = format!("{id} is now primary");
                window.push_notification(Notification::success(done), cx);
            }
            Some(Action::Remove) => {
                window.close_dialog(cx);
                (self.shark.update(cx, |s, cx| s.remove_issue(&name, &r, cx))).ok();
                let done = format!("Removed {id}");
                window.push_notification(Notification::success(done), cx);
            }
            _ => {}
        }
        cx.notify();
    }
}

impl Render for IssueDetails {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some((snapshot, primary, id)) = self.issue(cx) else {
            return div().into_any_element();
        };
        let connected = self
            .shark
            .upgrade()
            .zip(snapshot.as_ref())
            .is_some_and(|(shark, s)| shark.read(cx).linear_org_id() == Some(s.org.as_str()));
        let items = Self::actions(snapshot.is_some(), primary)
            .into_iter()
            .map(|a| match a {
                Action::Open => {
                    let icon = Some(IconName::ExternalLink);
                    line_item("issue-open", icon, "Open in Linear".into(), None, false)
                }
                Action::Refresh if self.refreshing => {
                    line_item("issue-refreshing", None, "Refreshing…".into(), None, false)
                }
                Action::Refresh => {
                    let icon = Some(IconName::RefreshCw);
                    line_item("issue-refresh", icon, "Refresh".into(), None, false)
                }
                Action::Primary => {
                    let hint = Some("Names new branches and comes first in context");
                    let icon = Some(IconName::Star);
                    line_item("issue-primary", icon, "Make primary".into(), hint, false)
                }
                Action::Remove => {
                    let icon = Some(IconName::Trash);
                    line_item("issue-remove", icon, "Remove attachment".into(), None, true)
                }
            });
        let this = cx.entity().downgrade();
        let actions = crate::ui::command(&self.state, window)
            .bordered(false)
            .searchable(false)
            .items(items.collect::<Vec<_>>())
            .on_confirm(move |at, window, cx| {
                this.update(cx, |d, cx| d.run(at.row, window, cx)).ok();
            });
        v_flex()
            .id("issue-details")
            .test_support()
            .gap_1()
            .child(match &snapshot {
                Some(s) => issue_summary(s, &self.description, connected, cx),
                None => v_flex()
                    .id("issue-missing")
                    .test_support()
                    .px_3()
                    .py_2()
                    .gap_1()
                    .child(div().text_base().font_weight(FontWeight::MEDIUM).child(id))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("This issue's snapshot is missing or unreadable. Refresh fetches it again."),
                    )
                    .into_any_element(),
            })
            .child(actions)
            .into_any_element()
    }
}

impl Shark {
    /// Whether anyone here uses Linear: connected, in whatever state, or with an issue still
    /// attached, whose chip stays readable and whose picker offers the way back. Everything Linear
    /// shows outside Settings hangs on this.
    pub(super) fn linear_in_use(&self) -> bool {
        self.state.linear.is_some()
            || (self.state.tasks.iter()).any(|w| w.linear.primary().is_some())
    }

    fn linear_blocked(&self) -> Option<Blocked> {
        match (&self.state.linear, self.linear_needs_attention()) {
            (None, _) => Some(Blocked::NotConnected),
            (_, Some(why)) => Some(Blocked::Attention(why.to_string())),
            _ => None,
        }
    }

    fn open_issue_picker(
        &mut self,
        agent: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let blocked = self.linear_blocked();
        let connection = (self.state.linear.clone(), self.linear_connection_epoch());
        let shark = cx.entity();
        let picker = cx.new(|cx| IssuePicker {
            state: cx.new(|cx| CommandState::new(window, cx)),
            agent,
            blocked,
            connection,
            query: String::new(),
            lists: None,
            cache: HashMap::new(),
            shown: String::new(),
            cursor: None,
            marks: vec![],
            seq: 0,
            flying: false,
            pending: false,
            sends: VecDeque::new(),
            wait: None,
            fetching: false,
            failure: None,
            shark: shark.downgrade(),
            _debounce: gpui_kit::Task::ready(()),
            _flight: gpui_kit::Task::ready(()),
            _wait: gpui_kit::Task::ready(()),
            _fetch: gpui_kit::Task::ready(()),
            _connection: cx.observe_in(&shark, window, |p: &mut IssuePicker, _, window, cx| {
                p.connection_changed(window, cx)
            }),
            settings: None,
        });
        let state = picker.read(cx).state.clone();
        let opened = picker.clone();
        window.open_dialog(cx, move |d, _, cx| menu_dialog(d, cx).child(picker.clone()));
        state.update(cx, |p, cx| p.focus(window, cx));
        // After this update: a request goes through Shark.
        window.defer(cx, move |window, cx| {
            opened.update(cx, |p, cx| p.send(window, cx))
        });
    }

    pub(super) fn attach_linear_issue(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_issue_picker(Some(name), window, cx)
    }

    pub(super) fn start_from_issue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_issue_picker(None, window, cx)
    }

    /// Adds `snapshots` to `name`, but not what's already there: only Refresh replaces a snapshot.
    fn attach_issues(
        &mut self,
        name: String,
        snapshots: Vec<Snapshot>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = self.task_title(&name);
        if self.archiving.contains(&name) {
            return notify_err(window, cx, format!("{title} is being deleted"));
        }
        let Some(ws) = self.task(&name) else { return };
        let (old, new): (Vec<_>, Vec<_>) = (snapshots.into_iter())
            .partition(|s| ws.linear.ordered().any(|x| *x == IssueRef::from(s)));
        for s in old {
            let msg = format!("{} is already attached to {title}", s.identifier);
            window.push_notification(msg, cx);
        }
        if !new.is_empty() {
            self.keep_snapshots(name, new, true, window, cx);
        }
    }

    /// Writes `snapshots` off the UI thread; once that worked the agent and its chats have them.
    fn keep_snapshots(
        &mut self,
        name: String,
        snapshots: Vec<Snapshot>,
        attach: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (agent, kept) = (name.clone(), snapshots.clone());
        let write = (cx.background_executor())
            .spawn(async move { (kept.iter()).try_for_each(|s| linear::put_snapshot(&agent, s)) });
        cx.spawn_in(window, async move |this, cx| {
            let written = write.await;
            this.update_in(cx, |s, window, cx| {
                let ids = snapshots.iter().map(|s| s.identifier.as_str());
                let ids = ids.collect::<Vec<_>>().join(", ");
                if let Err(e) = written {
                    return notify_err(window, cx, format!("Couldn't save {ids}: {e}"));
                }
                let Some(ws) = s.task_mut(&name) else {
                    return linear::delete_snapshots(&name); // deleted meanwhile
                };
                for snapshot in &snapshots {
                    linear::keep(&mut ws.issues, snapshot);
                    if attach {
                        ws.linear.attach(snapshot.into());
                    }
                }
                if attach {
                    let _ = s.state.save();
                }
                s.publish_task(&name, cx);
                let msg = match attach {
                    true => format!("Attached {ids} to {}", s.task_title(&name)),
                    false => format!("Refreshed {ids}"),
                };
                window.push_notification(Notification::success(msg), cx);
            })
            .ok();
        })
        .detach();
    }

    /// Why `s` can't be refreshed from here, if so. The connection decides, not the request.
    pub(super) fn refresh_refused(&self, s: &Snapshot) -> Option<String> {
        let id = &s.identifier;
        match self.linear_org_id() {
            None => Some(format!(
                "Connect Linear in Settings to refresh {id}. Its snapshot stays as it is."
            )),
            Some(org) if org != s.org => Some(format!(
                "{id} is from another Linear organization than the connected one, so it can't be refreshed. Its snapshot stays as it is."
            )),
            Some(_) => None,
        }
    }

    /// Manual, and the only way a snapshot changes. On failure the old one stays. Ends when
    /// Linear has answered. A missing snapshot comes back this way too: the reference is enough.
    pub(super) fn refresh_attached(
        &mut self,
        name: &str,
        r: &IssueRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Task<()> {
        let Some(ws) = self.task(name).filter(|w| w.linear.ordered().any(|x| x == r)) else {
            return gpui_kit::Task::ready(());
        };
        let old = r.of(&ws.issues).cloned().unwrap_or_else(|| Snapshot {
            org: r.org.clone(),
            id: r.issue.clone(),
            identifier: issue_label(ws, r),
            ..Snapshot::default()
        });
        if let Some(why) = self.refresh_refused(&old) {
            notify_err(window, cx, why);
            return gpui_kit::Task::ready(());
        }
        let id = old.identifier.clone();
        let task = self.linear_request(move |http, c| linear::refresh_issue(http, c, &old), cx);
        let (name, r) = (name.to_string(), r.clone());
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |s, window, cx| match result {
                Ok(fresh) => {
                    let attached = s
                        .task(&name)
                        .is_some_and(|w| w.linear.ordered().any(|x| *x == r));
                    if attached {
                        s.keep_snapshots(name, vec![fresh], false, window, cx);
                    }
                }
                Err(e) => notify_err(window, cx, format!("Couldn't refresh {id}: {e}")),
            })
            .ok();
        })
    }

    /// Never touches a branch or a title: `default_branch` only reads it for a first repository.
    pub(super) fn make_primary(&mut self, name: &str, r: &IssueRef, cx: &mut Context<Self>) {
        if let Some(ws) = self.task_mut(name) {
            ws.linear.make_primary(r);
            let _ = self.state.save();
            self.publish_task(name, cx);
        }
    }

    /// Stops it going into later turns; what chats were already told stays in them.
    pub(super) fn remove_issue(&mut self, name: &str, r: &IssueRef, cx: &mut Context<Self>) {
        let Some(ws) = self.task_mut(name) else { return };
        ws.linear.remove(r);
        ws.issues.retain(|s| IssueRef::from(s) != *r);
        let _ = self.state.save();
        self.publish_task(name, cx);
        let (agent, r) = (name.to_string(), r.clone());
        (cx.background_executor())
            .spawn(async move {
                // A failure leaves it in the file, unused, until the agent is deleted.
                let _ = linear::remove_snapshot(&agent, &r);
            })
            .detach();
    }

    /// An issue's snapshot and what can be done about it, keyboard first.
    pub(super) fn issue_details(
        &mut self,
        name: String,
        r: IssueRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.task(&name) else { return };
        let text = r
            .of(&ws.issues)
            .map_or(String::new(), |s| s.description.clone());
        let shark = cx.entity();
        let details = cx.new(|cx| IssueDetails {
            name,
            r,
            state: cx.new(|cx| CommandState::new(window, cx)),
            description: cx.new(|cx| TextViewState::markdown(&text, cx)),
            text,
            refreshing: false,
            shark: shark.downgrade(),
            _shark: cx.observe(&shark, |d: &mut IssueDetails, _, cx| {
                if let Some((Some(s), ..)) = d.issue(cx)
                    && s.description != d.text
                {
                    d.description
                        .update(cx, |m, cx| m.set_text(&s.description, cx));
                    d.text = s.description;
                }
                cx.notify();
            }),
        });
        let state = details.read(cx).state.clone();
        // Installs `searchable`, which `focus` goes by.
        RenderOnce::render(
            crate::ui::command(&state, window).searchable(false),
            window,
            cx,
        );
        window.open_dialog(cx, move |d, _, cx| {
            menu_dialog(d, cx).child(details.clone())
        });
        state.update(cx, |p, cx| p.focus(window, cx));
    }

    /// Show Linear issue: the one attached, or a choice.
    pub(super) fn pick_issue(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.task(&name) else { return };
        let refs: Vec<IssueRef> = ws.linear.ordered().cloned().collect();
        if let [only] = refs.as_slice() {
            return self.issue_details(name, only.clone(), window, cx);
        }
        if refs.is_empty() {
            return notify_err(window, cx, "No Linear issue is attached");
        }
        let items = (refs.iter())
            .map(|r| {
                CommandItem::new()
                    .label(issue_label(ws, r))
                    .icon(IconName::Tag)
            })
            .collect();
        open_menu(
            cx.entity().downgrade(),
            "Linear issue…",
            items,
            0,
            move |s, row, w, cx| s.issue_details(name.clone(), refs[row].clone(), w, cx),
            window,
            cx,
        );
    }

    /// Start from Linear issue's confirmation: nothing exists until it's accepted.
    fn preview_issue(&mut self, snapshot: Snapshot, window: &mut Window, cx: &mut Context<Self>) {
        let title = crate::chat::title_of(&snapshot.title);
        let title = cx.new(|cx| InputState::new(window, cx).default_value(title));
        let description = cx.new(|cx| TextViewState::markdown(&snapshot.description, cx));
        let request = planning_request(&snapshot);
        let request = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 10)
                .default_value(request)
        });
        let (field, shark) = (title.clone(), cx.entity().downgrade());
        let issue = snapshot.clone();
        let (edited, asked) = (title.clone(), request.clone());
        let start = Rc::new(move |window: &mut Window, cx: &mut App| {
            let title = edited.read(cx).value().trim().to_string();
            let title = match title.is_empty() {
                true => issue.title.clone(),
                false => title,
            };
            let first = asked.read(cx).value().trim().to_string();
            let first = (!first.is_empty()).then_some(first);
            window.close_dialog(cx);
            let issue = Some((title, issue.clone()));
            shark
                .update(cx, |s, cx| s.new_task(first, issue, window, cx))
                .ok();
        });
        window.open_dialog(cx, move |d, _, cx| {
            let (by_key, by_click) = (start.clone(), start.clone());
            let label = |text: &'static str| {
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(text)
            };
            crate::ui::dialog(d)
                .title("Start from Linear issue")
                .w(px(640.))
                .child(
                    v_flex()
                        .gap_2()
                        .child(issue_summary(&snapshot, &description, true, cx))
                        .child(
                            div()
                                .id("issue-plan-note")
                                .test_support()
                                .px_3()
                                .text_sm()
                                .child("The task starts in Plan mode, with no repository yet."),
                        )
                        .child(label("Title"))
                        .child(Input::new(&title))
                        .child(label("Planning request"))
                        .child(Textarea::new(&request)),
                )
                .footer(
                    Button::new("issue-start")
                        .primary()
                        .label("Start task")
                        .on_click(move |_, window, cx| by_click(window, cx)),
                )
                .on_ok(move |_, window, cx| {
                    by_key(window, cx);
                    false
                })
        });
        field.update(cx, |i, cx| {
            i.focus(window, cx);
            i.select_all(window, cx);
        });
    }

    /// `s` belongs to no connection here, so it can't refresh.
    fn disconnected(&self, s: &Snapshot) -> bool {
        self.linear_org_id() != Some(s.org.as_str())
    }

    /// An issue's row in Show attachments: what its chip says, in words, and its status icon.
    pub(super) fn issue_row(&self, ws: &Task, r: &IssueRef, theme: &Theme) -> (String, Icon) {
        let Some(s) = r.of(&ws.issues) else {
            let icon = Icon::new(IconName::TriangleAlert).text_color(theme.warning_foreground);
            return (format!("{} · snapshot missing", issue_label(ws, r)), icon);
        };
        let mut label = format!("{} {} · {}", s.identifier, s.title, s.status);
        if ws.linear.primary() == Some(r) {
            label += " · primary";
        }
        if self.disconnected(s) {
            label += " · disconnected";
        }
        (label, status_icon(&s.state_type, theme))
    }

    /// The header's one mark for `ws`'s disconnected issues, however many: the word on every chip
    /// pushed them out of the row. Their cards and details say it for each.
    pub(super) fn disconnected_chip(
        &self,
        ws: &Task,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let issues = (ws.linear.ordered()).filter_map(|r| r.of(&ws.issues));
        let n = issues.filter(|s| self.disconnected(s)).count();
        (n > 0).then(|| {
            let theme = cx.theme();
            let icon = Icon::new(IconName::CircleDashed).text_color(theme.muted_foreground);
            let tip = format!(
                "{n} disconnected issue{}: from another Linear organization, or Linear isn't connected. Opens Settings.",
                plural(n),
            );
            Button::new("issues-disconnected")
                .flex_none()
                .icon(icon)
                .accessibility_label(tip.clone())
                .tooltip(tip)
                .on_click(cx.listener(|s, _, w, cx| {
                    crate::settings::open_linear(&s.state, w, cx)
                }))
                .map(|b| crate::ui::toolbar("issues-disconnected", b))
                .into_any_element()
        })
    }

    /// The header's issue chips, primary first: state and identifier, a star on the primary.
    pub(super) fn issue_chips(
        &self,
        ws: &Task,
        quiet: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let primary = ws.linear.primary();
        (ws.linear.ordered())
            .map(|r| {
                let theme = cx.theme();
                let snapshot = r.of(&ws.issues);
                let disconnected = snapshot.is_some_and(|s| self.disconnected(s));
                let is_primary = Some(r) == primary;
                let label = issue_label(ws, r);
                let mut tip = snapshot
                    .map_or("Snapshot missing. Open it to refresh.".into(), |s| {
                        s.title.clone()
                    });
                if is_primary {
                    tip += " (primary)";
                }
                if disconnected {
                    tip += " (disconnected)";
                }
                let icon = snapshot.map_or(
                    Icon::new(IconName::TriangleAlert).text_color(theme.warning_foreground),
                    // Progress, not a problem: the shape says it, and color stays for what needs
                    // attention.
                    |s| status_icon(&s.state_type, theme).text_color(theme.muted_foreground),
                );
                let star = Icon::new(IconName::Star)
                    .with_size(px(12.))
                    .text_color(theme.muted_foreground);
                let (name, r) = (ws.name.clone(), r.clone());
                let id = SharedString::from(format!("issue-chip-{}", r.issue));
                let aria = format!("{label}: {tip}");
                let mut rows: Vec<_> = snapshot.map_or(
                    vec![("Snapshot", "Missing. Open it to refresh.".to_string())],
                    |s| {
                        [
                            ("Status", Some(&s.status)),
                            ("Project", s.project.as_ref()),
                            ("Assignee", s.assignee.as_ref()),
                        ]
                        .into_iter()
                        .filter_map(|(label, value)| Some((label, value?.clone())))
                        .collect()
                    },
                );
                if is_primary {
                    rows.push(("Attached as", "Primary".into()));
                }
                if disconnected {
                    rows.push(("Linear", "Disconnected".into()));
                }
                let card = ChipCard {
                    mark: icon.clone(),
                    title: snapshot.map_or(label.clone(), |s| s.title.clone()),
                    rows,
                };
                let chip = Self::chip(id.clone(), icon, label)
                    .when(is_primary, |c| c.child(star))
                    .accessibility_label(aria)
                    .on_click(cx.listener(move |s, _, w, cx| {
                        s.issue_details(name.clone(), r.clone(), w, cx)
                    }))
                    .map(|b| crate::ui::toolbar(id.clone(), b));
                card.around(&id, chip, quiet)
            })
            .collect()
    }
}
