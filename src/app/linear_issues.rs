//! Linear issues on an agent: attaching one, its details, and starting an agent from one. An
//! attachment is reference material: it starts no turn and waits for nothing, and chats hear of
//! it through `Workspace::context`. Nothing here writes to Linear.
use super::palette::{menu_dialog, open_headed};
use super::*;
use crate::linear::{self, Credential, Error, Http, IssueRef, Page, Target};
use gpui_kit::component::input::{Textarea, TextareaState};

/// What a row of the picker does.
#[derive(Clone, Copy)]
enum Row {
    Settings,
    Submit,
    Hit(usize),
    Next,
}

/// Attach Linear issue and Start from Linear issue: paste a reference or search, on Enter only.
pub(super) struct IssuePicker {
    state: Entity<CommandState>,
    /// The agent to attach to; None starts a new one.
    agent: Option<String>,
    connected: bool,
    query: String,
    /// The query `page` answers.
    searched: String,
    page: Page,
    /// The last failure; the query and the page stay.
    notice: Option<String>,
    busy: bool,
    shark: WeakEntity<Shark>,
    /// Closing the dialog drops the picker and this: what it would have attached is abandoned.
    _request: Option<Task<()>>,
}

impl IssuePicker {
    fn rows(&self) -> Vec<Row> {
        if !self.connected {
            return vec![Row::Settings];
        }
        let query = self.query.trim();
        let submit = !query.is_empty() && query != self.searched;
        (submit.then_some(Row::Submit).into_iter())
            .chain((0..self.page.hits.len()).map(Row::Hit))
            .chain(self.page.next.as_ref().map(|_| Row::Next))
            .collect()
    }

    fn item(&self, row: Row) -> CommandItem {
        let query = self.query.trim();
        let (icon, label) = match row {
            Row::Settings => (IconName::Settings, "Connect Linear in Settings".to_string()),
            Row::Submit => match linear::parse_issue(query) {
                Some(target) => (IconName::Search, format!("Look up {}", target.id)),
                None => (IconName::Search, format!("Search Linear for “{query}”")),
            },
            Row::Hit(i) => {
                let h = &self.page.hits[i];
                let label = format!("{} {} · {}", h.identifier, h.title, h.status);
                (IconName::Tag, label)
            }
            Row::Next => (IconName::ChevronRight, "Next page".to_string()),
        };
        CommandItem::new().label(label).icon(icon)
    }

    fn pick(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        match self.rows().get(row).copied() {
            Some(Row::Settings) => {
                window.close_dialog(cx);
                let shark = self.shark.clone();
                shark
                    .update(cx, |s, cx| {
                        crate::settings::open_linear(&s.state, window, cx)
                    })
                    .ok();
            }
            Some(Row::Submit) => match linear::parse_issue(&self.query) {
                Some(target) => self.fetch(target, window, cx),
                None => self.search(self.query.trim().to_string(), None, window, cx),
            },
            Some(Row::Hit(i)) => {
                let id = self.page.hits[i].identifier.clone();
                self.fetch(Target { id, org: None }, window, cx)
            }
            Some(Row::Next) => {
                let after = self.page.next.clone();
                self.search(self.searched.clone(), after, window, cx)
            }
            None => {}
        }
    }

    fn fetch(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        self.request(
            move |http, c| linear::get_issue(http, c, &target),
            |p, snapshot, window, cx| {
                window.close_dialog(cx);
                let agent = p.agent.clone();
                (p.shark.update(cx, |s, cx| match agent {
                    Some(name) => s.attach_issue(name, snapshot, window, cx),
                    None => s.preview_issue(snapshot, window, cx),
                }))
                .ok();
            },
            window,
            cx,
        );
    }

    fn search(
        &mut self,
        term: String,
        after: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let searched = term.clone();
        self.request(
            move |http, c| linear::search(http, c, &term, after.as_deref()),
            move |p, page, window, cx| {
                (p.searched, p.page) = (searched, page);
                p.state.update(cx, |s, cx| {
                    s.set_selected_index(Some(IndexPath::new(0)), window, cx)
                });
            },
            window,
            cx,
        );
    }

    /// One Linear read at a time; a failure leaves the query and the results where they are.
    fn request<T: Send + 'static>(
        &mut self,
        call: impl FnOnce(&Http, &Credential) -> Result<T, Error> + Send + 'static,
        done: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(task) = self.shark.update(cx, |s, cx| s.linear_request(call, cx)) else {
            return;
        };
        (self.busy, self.notice) = (true, None);
        self.state
            .update(cx, |s, cx| s.set_loading(true, window, cx));
        self._request = Some(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |p, window, cx| {
                p.busy = false;
                p.state.update(cx, |s, cx| s.set_loading(false, window, cx));
                match result {
                    Ok(value) => done(p, value, window, cx),
                    Err(e) => p.notice = Some(e.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }
}

impl Render for IssuePicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows();
        let items = rows.iter().map(|r| self.item(*r)).collect::<Vec<_>>();
        let hint = match !self.searched.is_empty() && self.searched == self.query.trim() {
            true => "No issues match",
            false => "Paste a Linear URL or identifier, or type to search",
        };
        let notice = match self.connected {
            true => self.notice.clone(),
            false => Some("Linear isn't connected".to_string()),
        };
        let danger = cx.theme().danger_foreground;
        let (q, pick) = (cx.entity().downgrade(), cx.entity().downgrade());
        Command::new(&self.state)
            .items(items)
            .filterable(false)
            .placeholder("Linear issue URL, identifier or search…")
            .empty(move |_, _, _| div().p_3().text_sm().child(hint))
            .when_some(notice, |c, notice| {
                c.footer(move |_, _, _| {
                    div()
                        .id("issue-notice")
                        .px_3()
                        .py_2()
                        .text_xs()
                        .text_color(danger)
                        .child(notice.clone())
                        .test_support()
                })
            })
            .on_query(move |query, _, cx| {
                q.update(cx, |p, cx| {
                    p.query = query.to_string();
                    cx.notify();
                })
                .ok();
            })
            .on_confirm(move |ix, window, cx| {
                pick.update(cx, |p, cx| p.pick(ix.row, window, cx)).ok();
            })
    }
}

/// The first request of an agent started from an issue: to plan, which is all it can do so far.
fn planning_request(s: &Snapshot) -> String {
    format!(
        "Read the attached Linear issue {} and draft a plan for it. List anything unclear before going further.",
        s.identifier
    )
}

/// An issue as details and previews show it, labelled as a snapshot.
fn issue_summary(s: &Snapshot, connected: bool, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let facts = [
        Some(&s.status),
        Some(&s.team),
        s.project.as_ref(),
        s.assignee.as_ref(),
    ];
    let facts: Vec<&str> = (facts.into_iter().flatten()).map(String::as_str).collect();
    let line = |text: String| {
        div()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(text)
    };
    v_flex()
        .id("issue-summary")
        .px_3()
        .py_2()
        .gap_1()
        .child(
            div()
                .text_sm()
                .child(format!("{} {}", s.identifier, s.title)),
        )
        .child(line(facts.join(" · ")))
        .child(line(format!("Snapshot from {}", s.refreshed())))
        .when(!connected, |d| {
            d.child(line(
                "Disconnected: from another Linear organization, or Linear isn't connected".into(),
            ))
        })
        .when(!s.description.is_empty(), |d| {
            d.child(
                div()
                    .id("issue-description")
                    .max_h(px(240.))
                    .overflow_y_scroll()
                    .text_xs()
                    .child(s.description.clone()),
            )
        })
        .test_support()
        .into_any_element()
}

fn issue_label(ws: &Workspace, r: &IssueRef) -> String {
    r.of(&ws.issues)
        .map_or("Linear issue".into(), |s| s.identifier.clone())
}

impl Shark {
    fn open_issue_picker(
        &mut self,
        agent: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (connected, shark) = (self.state.linear.is_some(), cx.entity().downgrade());
        let picker = cx.new(|cx| IssuePicker {
            state: cx.new(|cx| CommandState::new(window, cx)),
            agent,
            connected,
            query: String::new(),
            searched: String::new(),
            page: Page::default(),
            notice: None,
            busy: false,
            shark,
            _request: None,
        });
        let state = picker.read(cx).state.clone();
        window.open_dialog(cx, move |d, _, cx| menu_dialog(d, cx).child(picker.clone()));
        state.update(cx, |p, cx| p.focus(window, cx));
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

    /// Adds `snapshot` to `name` unless it's already there: only Refresh replaces a snapshot.
    fn attach_issue(
        &mut self,
        name: String,
        snapshot: Snapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = self.ws_title(&name);
        if self.archiving.contains(&name) {
            return notify_err(window, cx, format!("{title} is being deleted"));
        }
        let Some(ws) = self.ws(&name) else { return };
        let r = IssueRef::from(&snapshot);
        if ws.linear.ordered().any(|x| *x == r) {
            let msg = format!("{} is already attached to {title}", snapshot.identifier);
            return window.push_notification(msg, cx);
        }
        self.keep_snapshot(name, snapshot, true, window, cx);
    }

    /// Writes `snapshot` off the UI thread; once that worked the agent and its chats have it.
    fn keep_snapshot(
        &mut self,
        name: String,
        snapshot: Snapshot,
        attach: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (agent, kept) = (name.clone(), snapshot.clone());
        let write =
            (cx.background_executor()).spawn(async move { linear::put_snapshot(&agent, &kept) });
        cx.spawn_in(window, async move |this, cx| {
            let written = write.await;
            this.update_in(cx, |s, window, cx| {
                let id = &snapshot.identifier;
                if let Err(e) = written {
                    return notify_err(window, cx, format!("Couldn't save {id}: {e}"));
                }
                let Some(ws) = s.ws_mut(&name) else {
                    return linear::delete_snapshots(&name); // deleted meanwhile
                };
                linear::keep(&mut ws.issues, &snapshot);
                if attach {
                    ws.linear.attach((&snapshot).into());
                    let _ = s.state.save();
                }
                s.publish_workspace(&name, cx);
                let msg = match attach {
                    true => format!("Attached {id} to {}", s.ws_title(&name)),
                    false => format!("Refreshed {id}"),
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

    /// Manual, and the only way a snapshot changes. On failure the old one stays.
    pub(super) fn refresh_attached(
        &mut self,
        name: &str,
        r: &IssueRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(old) = self.ws(name).and_then(|w| r.of(&w.issues)).cloned() else {
            return;
        };
        if let Some(why) = self.refresh_refused(&old) {
            return notify_err(window, cx, why);
        }
        let id = old.identifier.clone();
        let task = self.linear_request(move |http, c| linear::refresh_issue(http, c, &old), cx);
        let (name, r) = (name.to_string(), r.clone());
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |s, window, cx| match result {
                Ok(fresh) => {
                    let attached = s.ws(&name).is_some_and(|w| r.of(&w.issues).is_some());
                    if attached {
                        s.keep_snapshot(name, fresh, false, window, cx);
                    }
                }
                Err(e) => notify_err(window, cx, format!("Couldn't refresh {id}: {e}")),
            })
            .ok();
        })
        .detach();
    }

    /// Never touches a branch or a title: `default_branch` only reads it for a first repository.
    pub(super) fn make_primary(&mut self, name: &str, r: &IssueRef, cx: &mut Context<Self>) {
        if let Some(ws) = self.ws_mut(name) {
            ws.linear.make_primary(r);
            let _ = self.state.save();
            self.publish_workspace(name, cx);
        }
    }

    /// Stops it going into later turns; what chats were already told stays in them.
    pub(super) fn remove_issue(&mut self, name: &str, r: &IssueRef, cx: &mut Context<Self>) {
        let Some(ws) = self.ws_mut(name) else { return };
        ws.linear.remove(r);
        ws.issues.retain(|s| IssueRef::from(s) != *r);
        let _ = self.state.save();
        self.publish_workspace(name, cx);
        let (agent, r) = (name.to_string(), r.clone());
        (cx.background_executor())
            .spawn(async move {
                // A failure leaves it in the file, unused, until the agent is deleted.
                let _ = linear::remove_snapshot(&agent, &r);
            })
            .detach();
    }

    /// An issue's snapshot and what can be done about it, as a keyboard menu.
    pub(super) fn issue_details(
        &mut self,
        name: String,
        r: IssueRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        type Act = fn(&mut Shark, &str, &IssueRef, &mut Window, &mut Context<Shark>);
        let Some(ws) = self.ws(&name) else { return };
        let snapshot = r.of(&ws.issues).cloned();
        let mut actions: Vec<(IconName, &str, Act)> = vec![];
        if ws.linear.primary() != Some(&r) {
            actions.push((IconName::Star, "Make primary", |s, n, r, _, cx| {
                s.make_primary(n, r, cx)
            }));
        }
        if snapshot.is_some() {
            actions.push((IconName::RefreshCw, "Refresh", |s, n, r, w, cx| {
                s.refresh_attached(n, r, w, cx)
            }));
            actions.push((
                IconName::ExternalLink,
                "Open in Linear",
                |s, n, r, _, cx| {
                    if let Some(issue) = s.ws(n).and_then(|w| r.of(&w.issues)) {
                        cx.open_url(&issue.url);
                    }
                },
            ));
        }
        actions.push((IconName::X, "Remove attachment", |s, n, r, _, cx| {
            s.remove_issue(n, r, cx)
        }));
        let items = (actions.iter())
            .map(|(icon, label, _)| CommandItem::new().label(*label).icon(*icon))
            .collect();
        let connected = snapshot
            .as_ref()
            .is_some_and(|s| self.linear_org_id() == Some(s.org.as_str()));
        let header = move |cx: &App| match &snapshot {
            Some(s) => issue_summary(s, connected, cx),
            None => div()
                .px_3()
                .py_2()
                .text_sm()
                .child("This issue's snapshot is missing or unreadable")
                .into_any_element(),
        };
        open_headed(
            cx.entity().downgrade(),
            "Linear issue…",
            vec![(None, items)],
            0,
            Some(header),
            move |s, row, w, cx| actions[row].2(s, &name, &r, w, cx),
            window,
            cx,
        );
    }

    /// Show Linear issue: the one attached, or a choice.
    pub(super) fn pick_issue(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.ws(&name) else { return };
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
                .update(cx, |s, cx| s.new_agent(first, issue, window, cx))
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
                        .child(issue_summary(&snapshot, true, cx))
                        .child(label("Title"))
                        .child(Input::new(&title))
                        .child(label("Planning request"))
                        .child(Textarea::new(&request)),
                )
                .footer(
                    Button::new("issue-start")
                        .primary()
                        .label("Start agent")
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

    /// The header's issue chips, primary first.
    pub(super) fn issue_chips(&self, ws: &Workspace, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let primary = ws.linear.primary();
        (ws.linear.ordered())
            .map(|r| {
                let snapshot = r.of(&ws.issues);
                let disconnected =
                    snapshot.is_some_and(|s| self.linear_org_id() != Some(s.org.as_str()));
                let mut label = issue_label(ws, r);
                let mut tip = snapshot.map_or("Snapshot missing".into(), |s| s.title.clone());
                if Some(r) == primary {
                    label += " · primary";
                }
                if disconnected {
                    label += " · disconnected";
                    tip += " (disconnected)";
                }
                let icon = match Some(r) == primary {
                    true => IconName::Star,
                    false => IconName::Tag,
                };
                let (name, r) = (ws.name.clone(), r.clone());
                let id = SharedString::from(format!("issue-chip-{}", r.issue));
                Self::chip(id, icon, label, cx.theme())
                    .cursor_pointer()
                    .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
                    .on_click(cx.listener(move |s, _, w, cx| {
                        s.issue_details(name.clone(), r.clone(), w, cx)
                    }))
                    .test_support()
                    .into_any_element()
            })
            .collect()
    }
}
