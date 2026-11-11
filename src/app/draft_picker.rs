//! Anchored context pickers for the task composer.
use super::task_draft::{BranchSpec, DraftRepo};
use super::*;
use crate::linear::{self, Hit};
use gpui_kit::component::dialog::Cancel;
use gpui_kit::component::tab::{Tab as PickerTab, TabBar};

pub(super) struct DraftPickerHost {
    view: AnyView,
    anchor: Point<Pixels>,
    previous_focus: Option<FocusHandle>,
}

#[derive(Clone)]
pub(super) struct SuggestionCache {
    repos: Vec<String>,
    sources: Vec<(String, PathBuf)>,
    epoch: Option<u64>,
    rows: Vec<Pick>,
}

impl Shark {
    pub(super) fn hold_draft_picker(
        &mut self,
        view: AnyView,
        anchor: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous_focus = self
            .draft_picker
            .take()
            .and_then(|p| p.previous_focus)
            .or_else(|| window.focused(cx));
        self.draft_picker = Some(DraftPickerHost {
            view,
            anchor,
            previous_focus,
        });
        cx.notify();
    }

    pub(super) fn close_draft_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = self.draft_picker.take() {
            self.restore_focus(panel.previous_focus, window, cx);
            cx.notify();
        }
    }

    pub(super) fn render_draft_picker(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let panel = self.draft_picker.as_ref()?;
        let viewport = window.viewport_size();
        let left = panel
            .anchor
            .x
            .max(px(8.))
            .min((viewport.width - px(440.)).max(px(8.)));
        let top = panel
            .anchor
            .y
            .max(px(8.))
            .min((viewport.height - px(460.)).max(px(8.)));
        Some(
            deferred(
                anchored().position(point(px(0.), px(0.))).child(
                    div()
                        .id("draft-picker-overlay")
                        .occlude()
                        .w(viewport.width)
                        .h(viewport.height)
                        .on_any_mouse_down(cx.listener(|s, _, w, cx| {
                            cx.stop_propagation();
                            s.close_draft_picker(w, cx);
                        }))
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        .child(
                            menu::chrome(cx.theme())
                                .absolute()
                                .left(left)
                                .top(top)
                                .w(px(440.))
                                .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                                .child(panel.view.clone()),
                        ),
                ),
            )
            .with_priority(101)
            .into_any_element(),
        )
    }

    pub(super) fn draft_add_picker(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sync_reference_completion(cx);
        if self.home.draft.starting {
            return;
        }
        let mut repos = self.state.repos.clone();
        repos.sort_by_key(|r| {
            (
                self.state
                    .last_repos
                    .iter()
                    .position(|n| n == &r.name)
                    .unwrap_or(usize::MAX),
                std::cmp::Reverse(
                    self.state
                        .tasks
                        .iter()
                        .filter(|t| t.repos.iter().any(|used| used.name == r.name))
                        .filter_map(|t| t.last_interacted_at)
                        .max(),
                ),
            )
        });
        let mut rows: Vec<Pick> = repos.iter().cloned().map(Pick::Repo).collect();
        for repo in &repos {
            rows.extend(
                complete::cached_repo_prs(&repo.name, &repo.path, true)
                    .into_iter()
                    .map(Pick::Pr),
            );
        }
        let shark = cx.entity().downgrade();
        if self.linear_in_use() {
            rows.extend(self.reference_hits().into_iter().map(Pick::Issue));
        }
        let agent_repos: Vec<_> = self
            .home
            .draft
            .repos
            .iter()
            .map(|r| r.repo.name.clone())
            .collect();
        let epoch = self.linear_connection_epoch();
        let sources: Vec<_> = repos
            .iter()
            .map(|r| (r.name.clone(), r.path.clone()))
            .collect();
        let cached = self
            .draft_suggestions
            .as_ref()
            .filter(|c| c.repos == agent_repos && c.sources == sources && c.epoch == epoch);
        let mut suggested = Vec::new();
        if let Some(cached) = cached {
            for pick in &cached.rows {
                if matches!(pick, Pick::Issue(_))
                    && self.linear_in_use()
                    && !rows.iter().any(|r| r.key() == pick.key())
                {
                    rows.push(pick.clone());
                }
                if let Some(current) = rows.iter().find(|r| r.key() == pick.key()) {
                    suggested.push(current.clone());
                }
            }
        }
        for pick in suggestions(&rows, &[], &[]) {
            if suggested.len() < 5 && !suggested.iter().any(|r| r.key() == pick.key()) {
                suggested.push(pick);
            }
        }
        let picker = cx.new(|cx| {
            let weak = cx.entity().downgrade();
            let keys = cx.intercept_keystrokes(move |event, w, cx| {
                let key = &event.keystroke;
                if key.key != "tab"
                    || (key.modifiers != Modifiers::default()
                        && key.modifiers != Modifiers::shift())
                {
                    return;
                }
                weak.update(cx, |p: &mut AddPicker, cx| {
                    if p.state.focus_handle(cx).is_focused(w) {
                        let count = AttachmentTab::ALL.len();
                        let step = if key.modifiers.shift { count - 1 } else { 1 };
                        p.change_tab(AttachmentTab::ALL[(p.tab as usize + step) % count], w, cx);
                        cx.stop_propagation();
                    }
                })
                .ok();
            });
            AddPicker {
                suggested: SuggestionCache {
                    repos: agent_repos.clone(),
                    sources,
                    epoch,
                    rows: suggested,
                },
                state: cx.new(|cx| CommandState::new(window, cx)),
                tab: AttachmentTab::All,
                pending_selection: None,
                rows,
                marks: Vec::new(),
                issues: Vec::new(),
                own_prs: Vec::new(),
                query_seq: 0,
                failure: None,
                attaching: false,
                shark,
                _keys: keys,
            }
        });
        let input = picker.read(cx).state.clone();
        self.hold_draft_picker(picker.clone().into(), position, window, cx);
        input.update(cx, |p, cx| p.focus(window, cx));
        if let Some(source) = repos.first().map(|r| r.path.clone()) {
            let refreshed_repos = repos.clone();
            picker.update(cx, |_, cx| {
                cx.spawn_in(window, async move |this, cx| {
                    let (login, refreshed) = cx
                        .background_executor()
                        .spawn(async move {
                            let login = git::cmd_bounded(
                                &source,
                                "gh",
                                &["api", "user", "--jq", ".login"],
                                10,
                            )
                            .ok();
                            let refreshed = crate::proc::par_map(&refreshed_repos, |repo| {
                                (
                                    repo.name.clone(),
                                    complete::fetch_repo_prs(&repo.name, &repo.path, true),
                                )
                            });
                            (login, refreshed)
                        })
                        .await;
                    this.update_in(cx, |p, window, cx| {
                        p.preserve_selection(window, cx);
                        for (repo, result) in refreshed {
                            if let Ok(prs) = result {
                                p.refresh_prs(&repo, prs);
                            }
                        }
                        if let Some(login) = login {
                            p.own_prs = p
                                .rows
                                .iter()
                                .filter_map(|r| match r {
                                    Pick::Pr(pr)
                                        if agent_repos.contains(&pr.repo)
                                            && pr
                                                .author
                                                .as_ref()
                                                .is_some_and(|a| a.login == login.trim()) =>
                                    {
                                        Some(pr.url.clone())
                                    }
                                    _ => None,
                                })
                                .collect();
                        }
                        p.remember_suggestions(cx);
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            });
        }
        if self.linear_in_use() {
            let request = self.linear_request(linear::lists, cx);
            picker.update(cx, |_, cx| {
                cx.spawn_in(window, async move |this, cx| {
                    let result = request.await;
                    this.update_in(cx, |p, window, cx| {
                        p.preserve_selection(window, cx);
                        match result {
                            Ok(lists) => {
                                p.issues = lists
                                    .assigned
                                    .iter()
                                    .filter(|h| h.state_type == "started")
                                    .take(5)
                                    .map(|h| h.id.clone())
                                    .collect();
                                for issue in lists.assigned.into_iter().chain(lists.recent) {
                                    p.rows.retain(
                                        |r| !matches!(r,Pick::Issue(known) if known.id == issue.id),
                                    );
                                    p.rows.push(Pick::Issue(issue));
                                }
                            }
                            Err(e) => p.failure = Some(e.to_string()),
                        }
                        p.remember_suggestions(cx);
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            });
        }
    }
}

#[derive(Clone)]
enum Pick {
    Repo(Repo),
    Pr(complete::Pr),
    Issue(Hit),
    Add,
    Files,
}
impl Pick {
    fn issue_target(&self) -> Option<linear::Target> {
        match self {
            Self::Issue(issue) => Some(linear::Target {
                id: issue.id.clone(),
                org: None,
            }),
            _ => None,
        }
    }

    fn key(&self) -> String {
        match self {
            Self::Repo(r) => format!("repo:{}", r.name),
            Self::Pr(p) => p.url.clone(),
            Self::Issue(i) => i.id.clone(),
            Self::Add => "add".into(),
            Self::Files => "files".into(),
        }
    }
    fn label(&self) -> String {
        match self {
            Self::Repo(r) => r.name.clone(),
            Self::Pr(p) => format!("#{} {}", p.number, p.title),
            Self::Issue(i) => format!("{} {}", i.identifier, i.title),
            Self::Add => "Add repository…".into(),
            Self::Files => "File…".into(),
        }
    }
    fn hint(&self) -> String {
        match self {
            Self::Repo(_) => "Repository".into(),
            Self::Pr(p) => format!("{} · {}", p.repo, p.status().label()),
            Self::Issue(i) => format!("Issue · {}", i.status),
            Self::Add | Self::Files => String::new(),
        }
    }
}

fn suggestions(rows: &[Pick], issues: &[String], own_prs: &[String]) -> Vec<Pick> {
    rows.iter()
        .filter(|r| matches!(r, Pick::Issue(i) if issues.contains(&i.id)))
        .chain(
            rows.iter()
                .filter(|r| matches!(r, Pick::Pr(p) if own_prs.contains(&p.url))),
        )
        .chain(rows.iter().filter(|r| matches!(r, Pick::Repo(_))))
        .take(5)
        .cloned()
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttachmentTab {
    All,
    Repositories,
    PullRequests,
    Issues,
    Files,
}

impl AttachmentTab {
    const ALL: [Self; 5] = [
        Self::All,
        Self::Repositories,
        Self::PullRequests,
        Self::Issues,
        Self::Files,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Repositories => "Repositories",
            Self::PullRequests => "Pull requests",
            Self::Issues => "Issues",
            Self::Files => "Files",
        }
    }

    fn includes(self, pick: &Pick) -> bool {
        match self {
            Self::All => true,
            Self::Repositories => matches!(pick, Pick::Repo(_) | Pick::Add),
            Self::PullRequests => matches!(pick, Pick::Pr(_)),
            Self::Issues => matches!(pick, Pick::Issue(_)),
            Self::Files => matches!(pick, Pick::Files),
        }
    }
}

enum PendingSelection {
    First,
    Key(Option<String>),
}

struct AddPicker {
    suggested: SuggestionCache,
    state: Entity<CommandState>,
    tab: AttachmentTab,
    pending_selection: Option<PendingSelection>,
    rows: Vec<Pick>,
    marks: Vec<String>,
    issues: Vec<String>,
    own_prs: Vec<String>,
    query_seq: u64,
    failure: Option<String>,
    attaching: bool,
    shark: WeakEntity<Shark>,
    _keys: Subscription,
}
impl AddPicker {
    fn refresh_prs(&mut self, repo: &str, prs: Vec<complete::Pr>) {
        self.rows.retain(|r| {
            !matches!(r, Pick::Pr(pr) if pr.repo == repo
                && (!self.marks.contains(&r.key()) || prs.iter().any(|next| next.url == pr.url)))
        });
        self.rows.extend(prs.into_iter().map(Pick::Pr));
    }

    fn remember_suggestions(&self, cx: &mut Context<Self>) {
        let mut next = self.suggested.clone();
        next.rows = suggestions(&self.rows, &self.issues, &self.own_prs);
        self.shark
            .update(cx, |s, _| s.draft_suggestions = Some(next))
            .ok();
    }

    fn preserve_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_selection.is_some() {
            return;
        }
        let key = self
            .state
            .read(cx)
            .selected_index()
            .and_then(|at| self.selected(at))
            .map(|r| r.key());
        self.pending_selection = Some(PendingSelection::Key(key));
        self.state
            .update(cx, |s, cx| s.set_selected_index(None, window, cx));
    }
    fn change_tab(&mut self, tab: AttachmentTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.state
                .update(cx, |s, cx| s.set_selected_index(None, window, cx));
            self.tab = tab;
            self.pending_selection = Some(PendingSelection::First);
            cx.notify();
        }
        self.state.update(cx, |s, cx| s.focus(window, cx));
    }

    fn groups(&self) -> Vec<(&'static str, Vec<Pick>)> {
        let groups = self.all_groups();
        if self.tab == AttachmentTab::All {
            return groups;
        }
        vec![(
            self.tab.label(),
            groups
                .into_iter()
                .flat_map(|(_, rows)| rows)
                .filter(|r| self.tab.includes(r))
                .collect(),
        )]
    }

    fn all_groups(&self) -> Vec<(&'static str, Vec<Pick>)> {
        // Refresh the next opening, not the rows under the user's pointer.
        let suggested = &self.suggested.rows;
        let mut groups = vec![("Suggested", suggested.clone())];
        for (name, kind) in [("Issues", 0), ("Pull requests", 1), ("Repositories", 2)] {
            let mut rows: Vec<_> = self
                .rows
                .iter()
                .filter(|r| match r {
                    Pick::Issue(_) => kind == 0,
                    Pick::Pr(_) => kind == 1,
                    Pick::Repo(_) => kind == 2,
                    Pick::Add | Pick::Files => false,
                })
                .filter(|r| !suggested.iter().any(|s| s.key() == r.key()))
                .cloned()
                .collect();
            if kind == 2 {
                rows.push(Pick::Add);
            }
            if !rows.is_empty() {
                groups.push((name, rows));
            }
        }
        groups.push(("Files", vec![Pick::Files]));
        groups.retain(|(_, r)| !r.is_empty());
        groups
    }
    fn selected(&self, at: IndexPath) -> Option<Pick> {
        self.groups().get(at.section)?.1.get(at.row).cloned()
    }
    fn allow_search(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(allowed) = self
            .shark
            .update(cx, |s, _| {
                s.linear_in_use().then(|| s.allow_reference_search())
            })
            .ok()
            .flatten()
        else {
            return false;
        };
        if !allowed {
            self.failure = Some("Search unavailable. Try again later.".into());
            cx.notify();
        }
        allowed
    }

    fn search(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.query_seq += 1;
        let seq = self.query_seq;
        let query = query.trim().to_string();
        if query.chars().count() < 2 {
            return;
        }
        let shark = self.shark.clone();
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(350))
                .await;
            let current = this
                .update_in(cx, |p, _, cx| {
                    if p.query_seq != seq {
                        return false;
                    }
                    p.allow_search(cx)
                })
                .unwrap_or(false);
            if !current {
                return;
            }
            let Ok((epoch, request)) = shark.update(cx, |s, cx| {
                (
                    s.linear_connection_epoch(),
                    s.linear_request(move |http, c| linear::find(http, c, &query), cx),
                )
            }) else {
                return;
            };
            let result = request.await;
            let current_connection = shark
                .update(cx, |s, _| {
                    if let Err(error) = &result {
                        s.reference_search_failed(epoch, error);
                    }
                    s.linear_connection_epoch() == epoch
                })
                .unwrap_or(false);
            if !current_connection {
                return;
            }
            this.update_in(cx, |p, window, cx| {
                if p.query_seq != seq {
                    return;
                }
                p.preserve_selection(window, cx);
                match result {
                    Ok(page) => {
                        for issue in page.hits {
                            if !p
                                .rows
                                .iter()
                                .any(|r| matches!(r, Pick::Issue(i) if i.id == issue.id))
                            {
                                p.rows.push(Pick::Issue(issue));
                            }
                        }
                    }
                    Err(e) => p.failure = Some(e.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
    fn attach(&mut self, at: IndexPath, window: &mut Window, cx: &mut Context<Self>) {
        if self.attaching {
            return;
        }
        let Some(selected) = self.selected(at) else {
            return;
        };
        if matches!(selected, Pick::Files) {
            self.shark
                .update(cx, |s, cx| {
                    s.close_draft_picker(window, cx);
                    s.pick_draft_files(window, cx);
                })
                .ok();
            return;
        }
        if matches!(selected, Pick::Add) {
            self.shark
                .update(cx, |s, cx| {
                    s.close_draft_picker(window, cx);
                    s.add_repo(window, cx);
                })
                .ok();
            return;
        }
        let picks: Vec<_> = if self.marks.is_empty() {
            vec![selected]
        } else {
            self.all_groups()
                .into_iter()
                .flat_map(|(_, rows)| rows)
                .filter(|r| self.marks.contains(&r.key()))
                .collect()
        };
        let targets: Vec<_> = picks.iter().filter_map(Pick::issue_target).collect();
        let request = (!targets.is_empty()).then(|| {
            self.shark.update(cx, |s, cx| {
                s.linear_request(
                    move |http, c| {
                        targets
                            .iter()
                            .map(|t| linear::get_issue(http, c, t))
                            .collect::<Result<Vec<_>, _>>()
                    },
                    cx,
                )
            })
        });
        self.attaching = true;
        cx.spawn_in(window, async move |this, cx| {
            let issues = match request {
                Some(Ok(task)) => task.await,
                Some(Err(_)) => return,
                None => Ok(Vec::new()),
            };
            this.update_in(cx, |p, w, cx| {
                p.attaching = false;
                let issues = match issues {
                    Ok(i) => i,
                    Err(e) => {
                        p.failure = Some(e.to_string());
                        cx.notify();
                        return;
                    }
                };
                p.shark
                    .update(cx, |s, cx| {
                        let registered = s.state.repos.clone();
                        let draft = &mut s.home.draft;
                        if !draft.starting {
                            for pick in &picks {
                                match pick {
                                    Pick::Repo(repo) => {
                                        if !draft.repos.iter().any(|r| r.repo.name == repo.name) {
                                            draft.repos.push(DraftRepo {
                                                repo: repo.clone(),
                                                pr_label: None,
                                                branch: BranchSpec::New {
                                                    base: String::new(),
                                                },
                                            });
                                            draft.repos_edited = true;
                                        }
                                    }
                                    Pick::Pr(pr) => {
                                        if let Some(repo) =
                                            registered.iter().find(|r| r.name == pr.repo)
                                        {
                                            let branch = BranchSpec::Pr(git::PullRequest {
                                                number: pr.number,
                                                url: pr.url.clone(),
                                                base: pr.base_ref_name.clone(),
                                            });
                                            if let Some(r) = draft
                                                .repos
                                                .iter_mut()
                                                .find(|r| r.repo.name == repo.name)
                                            {
                                                r.branch = branch;
                                                r.pr_label = Some(format!(
                                                    "#{} {} · {}",
                                                    pr.number,
                                                    pr.title,
                                                    pr.status().label()
                                                ));
                                            } else {
                                                draft.repos.push(DraftRepo {
                                                    repo: repo.clone(),
                                                    branch,
                                                    pr_label: Some(format!(
                                                        "#{} {} · {}",
                                                        pr.number,
                                                        pr.title,
                                                        pr.status().label()
                                                    )),
                                                });
                                            }
                                            draft.repos_edited = true;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            for issue in issues {
                                linear::keep(&mut draft.issues, &issue);
                            }
                        }
                        s.refresh_draft_sources(cx);
                        s.close_draft_picker(w, cx);
                        cx.notify();
                    })
                    .ok();
            })
            .ok();
        })
        .detach();
    }
}
impl Render for AddPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().downgrade();
        let typed = cx.entity().downgrade();
        let groups = self.groups();
        let command = groups
            .iter()
            .fold(
                crate::ui::command(&self.state, window),
                |c, (name, rows)| {
                    c.group(CommandGroup::new().label(*name).items(rows.iter().map(|r| {
                        let label = r.label();
                        let row_id = SharedString::from(format!("draft-context-{}", r.key()));
                        let hint = r.hint();
                        let marked = self.marks.contains(&r.key());
                        let icon = match r {
                            Pick::Repo(_) | Pick::Add => IconName::FolderGit2,
                            Pick::Files => IconName::Paperclip,
                            Pick::Pr(_) => IconName::GitPullRequest,
                            Pick::Issue(_) => IconName::Tag,
                        };
                        CommandItem::new()
                            .label(label.clone())
                            .keywords([hint.clone()])
                            .child(move |_, cx| {
                                h_flex()
                                    .id(row_id.clone())
                                    .test_support()
                                    .aria_label(format!(
                                        "{label}, {hint}, {}",
                                        if marked { "marked" } else { "unmarked" }
                                    ))
                                    .flex_1()
                                    .min_w_0()
                                    .gap_2()
                                    .child(if marked { "✓" } else { "  " })
                                    .child(Icon::new(icon).size_4())
                                    .child(div().flex_1().min_w_0().truncate().child(label.clone()))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(hint.clone()),
                                    )
                            })
                    })))
                },
            )
            .bordered(false)
            // Leave room for the tabs within the anchored menu's 460px height allowance.
            .max_h(px(350.))
            .placeholder("Search issues, pull requests, repositories…")
            .on_query(move |q, w, cx| {
                typed.update(cx, |p, cx| p.search(q, w, cx)).ok();
            })
            .on_confirm(move |at, w, cx| {
                entity.update(cx, |p, cx| p.attach(at, w, cx)).ok();
            });
        let command = RenderOnce::render(command, window, cx).into_any_element();
        if let Some(selection) = self.pending_selection.take() {
            let first = matches!(selection, PendingSelection::First);
            let at = match selection {
                PendingSelection::Key(key) => key.and_then(|key| {
                    groups.iter().enumerate().find_map(|(section, (_, rows))| {
                        rows.iter()
                            .position(|r| r.key() == key)
                            .map(|row| IndexPath::new(row).section(section))
                    })
                }),
                PendingSelection::First => None,
            };
            self.state.update(cx, |s, cx| {
                s.set_selected_index(at, window, cx);
                if first && s.matched_count() > 0 {
                    // Let CommandState resolve query matches against the newly installed groups.
                    'first: for (section, (_, rows)) in groups.iter().enumerate() {
                        for row in 0..rows.len() {
                            s.set_selected_index(
                                Some(IndexPath::new(row).section(section)),
                                window,
                                cx,
                            );
                            if s.selected_index().is_some() {
                                break 'first;
                            }
                        }
                    }
                }
            });
        }
        v_flex()
            .key_context("NewTask")
            .on_action(cx.listener(|p, _: &ToggleRepo, _, cx| {
                if let Some(at) = p.state.read(cx).selected_index() {
                    if let Some(r) = p.selected(at) {
                        if !matches!(r, Pick::Add | Pick::Files) {
                            let key = r.key();
                            if p.marks.contains(&key) {
                                p.marks.retain(|k| k != &key);
                            } else {
                                p.marks.push(key);
                            }
                            cx.notify();
                        }
                    }
                }
            }))
            .on_action(cx.listener(|p, _: &Cancel, w, cx| {
                p.shark.update(cx, |s, cx| s.close_draft_picker(w, cx)).ok();
            }))
            .child(
                TabBar::new("draft-picker-tabs")
                    .underline()
                    .small()
                    .px_3()
                    .selected_index(self.tab as usize)
                    .children(AttachmentTab::ALL.map(|tab| PickerTab::new().label(tab.label())))
                    .on_click(cx.listener(|p, index: &usize, w, cx| {
                        p.change_tab(AttachmentTab::ALL[*index], w, cx)
                    })),
            )
            .child(command)
            .children(
                self.failure
                    .clone()
                    .map(|e| div().px_3().text_sm().child(e)),
            )
            .child(
                crate::ui::key_footer(
                    &[
                        ("up down", "Move"),
                        ("tab", "Switch"),
                        ("space", "Mark"),
                        ("enter", "Attach"),
                        ("escape", "Close"),
                    ],
                    None,
                )
                .gap_2()
                .flex_wrap()
                .id("draft-picker-footer")
                .test_support(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    #[cfg(feature = "test-support")]
    use gpui_kit::test::TestWindowExt;

    #[cfg(feature = "test-support")]
    fn picker(shark: &Entity<Shark>, w: &mut Window, cx: &mut App) -> Entity<AddPicker> {
        shark.update(cx, |s, cx| {
            s.draft_add_picker(point(px(100.), px(100.)), w, cx);
            s.draft_picker
                .as_ref()
                .unwrap()
                .view
                .clone()
                .downcast::<AddPicker>()
                .ok()
                .unwrap()
        })
    }

    #[cfg(feature = "test-support")]
    fn pr(number: u64) -> Pick {
        Pick::Pr(serde_json::from_value(serde_json::json!({
            "repo": "web", "number": number, "title": format!("Web fix {number}"),
            "url": format!("https://github.com/test/web/pull/{number}"), "state": "OPEN", "baseRefName": "main"
        })).unwrap())
    }

    #[cfg(feature = "test-support")]
    fn selected_key(p: &AddPicker, cx: &App) -> Option<String> {
        p.state
            .read(cx)
            .selected_index()
            .and_then(|at| p.selected(at))
            .map(|r| r.key())
    }

    #[cfg(feature = "test-support")]
    fn repositories(home: &std::path::Path) -> Vec<Repo> {
        ["api", "web"]
            .map(|name| Repo {
                name: name.into(),
                path: home.join(name),
            })
            .to_vec()
    }

    #[cfg(feature = "test-support")]
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn attachment_tabs_are_visible_even_without_results(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::test::TestWindowExt;
        let _turn = super::super::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        super::super::ui_tests::test_home();
        let (window, shark) =
            super::super::ui_tests::boot(cx, State::default(), WindowOptions::default());
        window
            .update(cx, |_, w, cx| {
                shark.update(cx, |s, cx| {
                    s.enter_home(w, cx);
                    s.draft_add_picker(point(px(100.), px(100.)), w, cx);
                });
                w.render_frame(cx);
                for tab in AttachmentTab::ALL {
                    assert!(
                        w.within("draft-picker-tabs")
                            .find_by_label(tab.label())
                            .visible()
                    );
                }
            })
            .unwrap();
    }

    #[cfg(feature = "test-support")]
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn draft_picker_stays_inside_bottom_edge(cx: &mut gpui_kit::TestAppContext) {
        let _turn = super::super::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = super::super::ui_tests::test_home();
        let (window, shark) = super::super::ui_tests::boot(
            cx,
            State {
                repos: repositories(&home),
                ..Default::default()
            },
            WindowOptions::default(),
        );
        window
            .update(cx, |_, w, cx| {
                let p = picker(&shark, w, cx);
                p.update(cx, |p, cx| {
                    p.rows.extend((1..20).map(pr));
                    cx.notify();
                });
                shark.update(cx, |s, cx| {
                    s.draft_picker.as_mut().unwrap().anchor.y = w.viewport_size().height;
                    cx.notify();
                });
                w.render_frame(cx);
                w.render_frame(cx);
                let footer = w.find("draft-picker-footer").bounds();
                assert!(
                    footer.bottom() <= w.viewport_size().height,
                    "footer must fit inside the window: {footer:?}"
                );
            })
            .unwrap();
    }

    #[cfg(feature = "test-support")]
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn category_tabs_include_suggestions_once_and_keep_special_actions(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let _turn = super::super::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = super::super::ui_tests::test_home();
        let (window, shark) = super::super::ui_tests::boot(
            cx,
            State {
                repos: repositories(&home),
                ..Default::default()
            },
            WindowOptions::default(),
        );
        window
            .update(cx, |_, w, cx| {
                let picker = picker(&shark, w, cx);
                picker.update(cx, |p, _| {
                    let issue = Pick::Issue(Hit {
                        id: "issue".into(),
                        identifier: "SHA-1".into(),
                        title: "Fix issue".into(),
                        url: String::new(),
                        status: "Started".into(),
                        state_type: "started".into(),
                        priority: 0,
                        team: String::new(),
                        assignee: None,
                        updated_at: String::new(),
                    });
                    p.rows.extend([pr(1), issue.clone()]);
                    p.suggested.rows.extend([pr(1), issue]);
                    assert_eq!(p.groups()[0].0, "Suggested");
                });
                for (tab, expected) in [
                    (
                        AttachmentTab::Repositories,
                        vec!["repo:api".to_string(), "repo:web".into(), "add".into()],
                    ),
                    (AttachmentTab::PullRequests, vec![pr(1).key()]),
                    (AttachmentTab::Issues, vec!["issue".into()]),
                    (AttachmentTab::Files, vec!["files".into()]),
                ] {
                    picker.update(cx, |p, cx| p.change_tab(tab, w, cx));
                    w.render_frame(cx);
                    let groups = picker.read(cx).groups();
                    assert_eq!(groups.len(), 1);
                    assert_eq!(groups[0].0, tab.label());
                    assert_eq!(
                        groups[0].1.iter().map(Pick::key).collect::<Vec<_>>(),
                        expected
                    );
                    assert_eq!(selected_key(picker.read(cx), cx), expected.first().cloned());
                }
            })
            .unwrap();
    }

    #[cfg(feature = "test-support")]
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn tabs_click_cycle_wrap_and_reopen_with_search_focus(cx: &mut gpui_kit::TestAppContext) {
        let _turn = super::super::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = super::super::ui_tests::test_home();
        let (window, shark) = super::super::ui_tests::boot(
            cx,
            State {
                repos: repositories(&home),
                agents: vec![crate::state::Agent {
                    slug: "designer".into(),
                    name: "Designer".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            WindowOptions::default(),
        );
        window
            .update(cx, |_, w, cx| {
                for agent in [false, true] {
                    shark.update(cx, |s, cx| {
                        if agent {
                            s.prefill_agent_task("designer", w, cx);
                        } else {
                            s.enter_home(w, cx);
                        }
                    });
                    let p = picker(&shark, w, cx);
                    p.update(cx, |p, _| p.rows.push(pr(1)));
                    w.render_frame(cx);
                    let focus = p.read(cx).state.focus_handle(cx);
                    w.input("web", cx);
                    for (tab, expected) in [
                        (AttachmentTab::Repositories, Some("repo:web".to_string())),
                        (AttachmentTab::PullRequests, Some(pr(1).key())),
                        (AttachmentTab::Issues, None),
                        (AttachmentTab::Files, None),
                        (AttachmentTab::All, Some("repo:web".into())),
                    ] {
                        w.press("tab", cx);
                        assert_eq!(p.read(cx).tab, tab);
                        assert_eq!(selected_key(p.read(cx), cx), expected);
                        assert_eq!(p.read(cx).state.read(cx).query(cx).as_ref(), "web");
                        assert!(focus.is_focused(w));
                    }
                    for tab in AttachmentTab::ALL.into_iter().rev() {
                        w.press("shift-tab", cx);
                        assert_eq!(p.read(cx).tab, tab);
                        assert!(focus.is_focused(w));
                    }
                    for tab in AttachmentTab::ALL {
                        w.within("draft-picker-tabs").click(tab as usize, cx);
                        assert_eq!(p.read(cx).tab, tab);
                        assert!(focus.is_focused(w));
                        assert_eq!(p.read(cx).state.read(cx).query(cx).as_ref(), "web");
                    }
                    for key in ["alt-tab", "ctrl-tab", "cmd-tab", "fn-tab", "ctrl-shift-tab"] {
                        w.press(key, cx);
                        assert_eq!(p.read(cx).tab, AttachmentTab::Files, "{key}");
                    }
                    shark.update(cx, |s, cx| s.close_draft_picker(w, cx));
                    let reopened = picker(&shark, w, cx);
                    w.render_frame(cx);
                    assert_eq!(reopened.read(cx).tab, AttachmentTab::All);
                    assert!(reopened.read(cx).state.read(cx).query(cx).is_empty());
                    shark.update(cx, |s, cx| s.close_draft_picker(w, cx));
                }
            })
            .unwrap();
    }

    #[cfg(feature = "test-support")]
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn marked_attachments_survive_category_and_query_changes(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let _turn = super::super::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = super::super::ui_tests::test_home();
        let (window, shark) = super::super::ui_tests::boot(
            cx,
            State {
                repos: repositories(&home),
                ..Default::default()
            },
            WindowOptions::default(),
        );
        window
            .update(cx, |_, w, cx| {
                shark.update(cx, |s, cx| s.enter_home(w, cx));
                let p = picker(&shark, w, cx);
                p.update(cx, |p, _| {
                    p.rows.push(pr(1));
                    p.suggested.rows.push(pr(1));
                });
                w.render_frame(cx);
                w.press("space", cx);
                w.press("tab", cx);
                w.press("tab", cx);
                w.press("space", cx);
                assert_eq!(p.read(cx).marks, vec!["repo:api".to_string(), pr(1).key()]);
                w.press("shift-tab", cx);
                w.input("api", cx);
                assert_eq!(selected_key(p.read(cx), cx), Some("repo:api".into()));
                w.press("enter", cx);
            })
            .unwrap();
        cx.run_until_parked();
        cx.read(|cx| {
            let s = shark.read(cx);
            assert!(s.draft_picker.is_none());
            assert_eq!(s.home.draft.repos.len(), 2);
            assert_eq!(s.home.draft.repos[0].repo.name, "api");
            assert_eq!(s.home.draft.repos[1].repo.name, "web");
            assert!(matches!(&s.home.draft.repos[1].branch, BranchSpec::Pr(pr) if pr.number == 1));
            assert!(s.state.tasks.is_empty());
        });
    }

    #[cfg(feature = "test-support")]
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn category_actions_open_existing_path_prompts_without_attaching_marks(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let _turn = super::super::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = super::super::ui_tests::test_home();
        let (window, shark) = super::super::ui_tests::boot(
            cx,
            State {
                repos: repositories(&home),
                ..Default::default()
            },
            WindowOptions::default(),
        );
        for tab in [AttachmentTab::Repositories, AttachmentTab::Files] {
            window
                .update(cx, |_, w, cx| {
                    shark.update(cx, |s, cx| s.enter_home(w, cx));
                    let p = picker(&shark, w, cx);
                    w.render_frame(cx);
                    w.press("space", cx);
                    p.update(cx, |p, cx| p.change_tab(tab, w, cx));
                    w.render_frame(cx);
                    if tab == AttachmentTab::Repositories {
                        w.input("Add", cx);
                    }
                    w.press("enter", cx);
                })
                .unwrap();
            cx.run_until_parked();
            cx.read(|cx| assert!(shark.read(cx).draft_picker.is_none()));
            assert!(cx.did_prompt_for_paths());
            cx.simulate_path_prompt_response(|options| {
                assert_eq!(options.files, tab == AttachmentTab::Files);
                assert_eq!(options.directories, tab == AttachmentTab::Repositories);
                assert!(options.multiple);
                None
            });
            cx.run_until_parked();
            cx.read(|cx| assert!(shark.read(cx).home.draft.repos.is_empty()));
        }
    }

    #[cfg(feature = "test-support")]
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn refresh_keeps_tab_query_and_identity_when_rows_reorder_or_disappear(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let _turn = super::super::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = super::super::ui_tests::test_home();
        let (window, shark) = super::super::ui_tests::boot(
            cx,
            State {
                repos: repositories(&home),
                ..Default::default()
            },
            WindowOptions::default(),
        );
        for remove in [true, false] {
            window
                .update(cx, |_, w, cx| {
                    shark.update(cx, |s, cx| s.enter_home(w, cx));
                    let p = picker(&shark, w, cx);
                    p.update(cx, |p, cx| {
                        p.rows.extend([pr(1), pr(2)]);
                        p.change_tab(AttachmentTab::PullRequests, w, cx);
                    });
                    w.render_frame(cx);
                    w.input("fix", cx);
                    w.press("down", cx);
                    assert_eq!(selected_key(p.read(cx), cx), Some(pr(2).key()));
                    p.update(cx, |p, cx| {
                        p.preserve_selection(w, cx);
                        p.rows.reverse();
                        if remove {
                            p.rows.retain(|r| r.key() != pr(2).key());
                        }
                        assert!(p.state.read(cx).selected_index().is_none());
                        cx.notify();
                    });
                    w.render_frame(cx);
                    assert_eq!(p.read(cx).tab, AttachmentTab::PullRequests);
                    assert_eq!(p.read(cx).state.read(cx).query(cx).as_ref(), "fix");
                    assert_eq!(selected_key(p.read(cx), cx), (!remove).then(|| pr(2).key()));
                    w.press("enter", cx);
                    if remove {
                        assert!(shark.read(cx).draft_picker.is_some());
                        assert!(shark.read(cx).home.draft.repos.is_empty());
                        shark.update(cx, |s, cx| s.close_draft_picker(w, cx));
                    }
                })
                .unwrap();
            cx.run_until_parked();
        }
        cx.read(|cx| {
            assert!(matches!(&shark.read(cx).home.draft.repos[0].branch, BranchSpec::Pr(pr) if pr.number == 2));
        });
    }

    #[cfg(feature = "test-support")]
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn background_suggestions_do_not_move_the_highlighted_repository(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use gpui_kit::test::TestWindowExt;
        let _turn = super::super::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = super::super::ui_tests::test_home();
        let (window, shark) = super::super::ui_tests::boot(
            cx,
            State {
                repos: vec![Repo {
                    name: "api".into(),
                    path: home.join("api"),
                }],
                ..Default::default()
            },
            WindowOptions::default(),
        );
        let picker = window
            .update(cx, |_, w, cx| {
                shark.update(cx, |s, cx| {
                    s.enter_home(w, cx);
                    s.draft_add_picker(point(px(100.), px(100.)), w, cx);
                    s.draft_picker
                        .as_ref()
                        .unwrap()
                        .view
                        .clone()
                        .downcast::<AddPicker>()
                        .ok()
                        .unwrap()
                })
            })
            .unwrap();
        window
            .update(cx, |_, w, cx| {
                w.render_frame(cx);
                let before = picker.read(cx).state.read(cx).selected_index().unwrap();
                assert_eq!(picker.read(cx).selected(before).unwrap().key(), "repo:api");
                let position = w.find("draft-context-repo:api").bounds().origin;
                picker.update(cx, |p, cx| {
                    p.preserve_selection(w, cx);
                    for n in 0..5 {
                        let issue = Hit {
                            id: format!("issue-{n}"),
                            identifier: format!("SHA-{n}"),
                            title: "Assigned issue".into(),
                            url: String::new(),
                            status: "In progress".into(),
                            state_type: "started".into(),
                            priority: 0,
                            team: String::new(),
                            assignee: None,
                            updated_at: String::new(),
                        };
                        p.issues.push(issue.id.clone());
                        p.rows.push(Pick::Issue(issue));
                    }
                    p.remember_suggestions(cx);
                    cx.notify();
                });
                w.render_frame(cx);
                let p = picker.read(cx);
                let after = p.state.read(cx).selected_index().unwrap();
                assert_eq!(
                    after, before,
                    "background results must not move the selected repository to another group"
                );
                assert_eq!(p.selected(after).unwrap().key(), "repo:api");
                assert_eq!(w.find("draft-context-repo:api").bounds().origin, position);
                assert!(
                    p.groups()
                        .iter()
                        .any(|(name, rows)| *name == "Issues" && rows.len() == 5)
                );
                assert_eq!(
                    shark.read(cx).draft_suggestions.as_ref().unwrap().rows[0].key(),
                    "issue-0"
                );
                // A refreshed source may omit a displayed suggestion; marking must still attach it.
                picker.update(cx, |p, cx| {
                    p.rows.retain(|r| r.key() != "repo:api");
                    p.marks.push("repo:api".into());
                    p.attach(before, w, cx);
                });
            })
            .unwrap();
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(shark.read(cx).home.draft.repos[0].repo.name, "api");
            assert!(shark.read(cx).state.tasks.is_empty());
        });
    }

    #[cfg(feature = "test-support")]
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn refreshed_suggestions_apply_on_reopen_and_expire_with_the_connection(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let _turn = super::super::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = super::super::ui_tests::test_home();
        let (window, shark) = super::super::ui_tests::boot(
            cx,
            State {
                repos: ["api", "web"]
                    .iter()
                    .map(|name| Repo {
                        name: (*name).into(),
                        path: home.join(name),
                    })
                    .collect(),
                ..Default::default()
            },
            WindowOptions::default(),
        );
        window
            .update(cx, |_, w, cx| {
                shark.update(cx, |s, cx| {
                    s.enter_home(w, cx);
                    s.draft_add_picker(point(px(100.), px(100.)), w, cx);
                });
                let picker = shark
                    .read(cx)
                    .draft_picker
                    .as_ref()
                    .unwrap()
                    .view
                    .clone()
                    .downcast::<AddPicker>()
                    .ok()
                    .unwrap();
                picker.update(cx, |p, cx| {
                    p.rows.swap(0, 1);
                    p.remember_suggestions(cx);
                    assert_eq!(p.groups()[0].1[0].key(), "repo:api");
                });
                shark.update(cx, |s, cx| {
                    s.close_draft_picker(w, cx);
                    s.draft_add_picker(point(px(100.), px(100.)), w, cx);
                    let reopened = s
                        .draft_picker
                        .as_ref()
                        .unwrap()
                        .view
                        .clone()
                        .downcast::<AddPicker>()
                        .ok()
                        .unwrap();
                    assert_eq!(reopened.read(cx).groups()[0].1[0].key(), "repo:web");
                    s.draft_suggestions.as_mut().unwrap().epoch = Some(u64::MAX);
                    s.close_draft_picker(w, cx);
                    s.draft_add_picker(point(px(100.), px(100.)), w, cx);
                    let reopened = s
                        .draft_picker
                        .as_ref()
                        .unwrap()
                        .view
                        .clone()
                        .downcast::<AddPicker>()
                        .ok()
                        .unwrap();
                    assert_eq!(reopened.read(cx).groups()[0].1[0].key(), "repo:api");
                });
            })
            .unwrap();
    }

    #[cfg(feature = "test-support")]
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn local_picker_search_without_linear_stays_silent(cx: &mut gpui_kit::TestAppContext) {
        let _turn = super::super::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        super::super::ui_tests::test_home();
        let (window, shark) =
            super::super::ui_tests::boot(cx, State::default(), WindowOptions::default());
        window
            .update(cx, |_, w, cx| {
                let picker = picker(&shark, w, cx);
                picker.update(cx, |p, cx| {
                    assert!(!p.allow_search(cx));
                    assert!(
                        p.failure.is_none(),
                        "local filtering must not show a Linear connection error"
                    );
                });
            })
            .unwrap();
    }

    #[cfg(feature = "test-support")]
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn reopened_picker_obeys_shared_search_budget(cx: &mut gpui_kit::TestAppContext) {
        let _turn = super::super::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        super::super::ui_tests::test_home();
        let (window, shark) =
            super::super::ui_tests::boot(cx, State::default(), WindowOptions::default());
        window
            .update(cx, |_, w, cx| {
                shark.update(cx, |s, cx| {
                    s.linear_connected(
                        "key",
                        linear::Connection {
                            org_id: "workspace-id".into(),
                            org: "workspace".into(),
                            user: "Ada".into(),
                            method: linear::Method::ApiKey,
                        },
                        &|_| Err(linear::Error::Http(503)),
                        cx,
                    );
                    for _ in 0..25 {
                        assert!(s.allow_reference_search());
                    }
                    s.enter_home(w, cx);
                });
                let first = picker(&shark, w, cx);
                assert!(!first.update(cx, |p, cx| p.allow_search(cx)));
                shark.update(cx, |s, cx| s.close_draft_picker(w, cx));
                let reopened = picker(&shark, w, cx);
                assert!(!reopened.update(cx, |p, cx| p.allow_search(cx)));
            })
            .unwrap();
    }

    #[cfg(feature = "test-support")]
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn marked_pr_survives_background_refresh_omission(cx: &mut gpui_kit::TestAppContext) {
        let _turn = super::super::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = super::super::ui_tests::test_home();
        let (window, shark) = super::super::ui_tests::boot(
            cx,
            State {
                repos: repositories(&home),
                ..Default::default()
            },
            WindowOptions::default(),
        );
        window
            .update(cx, |_, w, cx| {
                shark.update(cx, |s, cx| s.enter_home(w, cx));
                let picker = picker(&shark, w, cx);
                picker.update(cx, |p, cx| {
                    p.rows.push(pr(42));
                    p.change_tab(AttachmentTab::PullRequests, w, cx);
                });
                w.render_frame(cx);
                w.press("space", cx);
                assert_eq!(picker.read(cx).marks, vec![pr(42).key()]);
                picker.update(cx, |p, cx| {
                    p.preserve_selection(w, cx);
                    p.refresh_prs("web", vec![]);
                    cx.notify();
                });
                w.render_frame(cx);
                w.press("enter", cx);
            })
            .unwrap();
        cx.run_until_parked();
        cx.read(|cx| {
            let draft = &shark.read(cx).home.draft;
            assert_eq!(draft.repos.len(), 1);
            assert!(matches!(&draft.repos[0].branch, BranchSpec::Pr(pr) if pr.number == 42));
        });
    }

    #[cfg(feature = "test-support")]
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn attaching_repository_and_pr_refreshes_composer_sources(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let _turn = super::super::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = super::super::ui_tests::test_home();
        let (window, shark) = super::super::ui_tests::boot(
            cx,
            State {
                repos: repositories(&home),
                ..Default::default()
            },
            WindowOptions::default(),
        );
        for pull_request in [false, true] {
            window
                .update(cx, |_, w, cx| {
                    shark.update(cx, |s, cx| s.enter_home(w, cx));
                    let picker = picker(&shark, w, cx);
                    picker.update(cx, |p, cx| {
                        if pull_request {
                            p.rows.push(pr(42));
                            p.change_tab(AttachmentTab::PullRequests, w, cx);
                        } else {
                            p.change_tab(AttachmentTab::Repositories, w, cx);
                        }
                    });
                    w.render_frame(cx);
                    let p = picker.read(cx);
                    let at = if pull_request {
                        p.state.read(cx).selected_index().unwrap()
                    } else {
                        IndexPath::new(1)
                    };
                    picker.update(cx, |p, cx| p.attach(at, w, cx));
                })
                .unwrap();
            cx.run_until_parked();
            cx.read(|cx| {
                let sources = shark.read(cx).home.composer.read(cx).sources_for_test();
                assert_eq!(sources.len(), 1);
                if pull_request {
                    let source = sources.iter().find(|s| s.name == "web").unwrap();
                    assert_eq!(source.start_ref, "refs/pull/42/head");
                    assert_eq!(source.pr.as_ref().unwrap().number, 42);
                } else {
                    assert_eq!(sources[0].name, "web");
                }
            });
        }
    }

    #[test]
    fn issue_attachment_keeps_identity_when_workspace_identifiers_overlap() {
        let issue = Pick::Issue(Hit {
            id: "6d43a2b1-8042-4a16-9c7a-41f31b566103".into(),
            identifier: "ENG-123".into(),
            url: "https://linear.app/original-workspace/issue/ENG-123/fix".into(),
            title: String::new(),
            status: String::new(),
            state_type: String::new(),
            priority: 0,
            team: String::new(),
            assignee: None,
            updated_at: String::new(),
        });
        assert_eq!(
            issue.issue_target().unwrap().id,
            "6d43a2b1-8042-4a16-9c7a-41f31b566103"
        );
    }

    #[test]
    fn suggestions_prioritize_started_assigned_issues_and_cap_at_five() {
        let rows: Vec<_> = (0..8)
            .map(|n| {
                Pick::Issue(Hit {
                    id: n.to_string(),
                    identifier: format!("SHA-{n}"),
                    title: String::new(),
                    url: String::new(),
                    status: String::new(),
                    state_type: "started".into(),
                    priority: 0,
                    team: String::new(),
                    assignee: None,
                    updated_at: String::new(),
                })
            })
            .collect();
        let assigned: Vec<_> = (1..8).map(|n| n.to_string()).collect();
        let picked = suggestions(&rows, &assigned, &[]);
        assert_eq!(
            picked.iter().map(Pick::key).collect::<Vec<_>>(),
            vec!["1", "2", "3", "4", "5"]
        );
        assert!(suggestions(&rows, &[], &[]).is_empty());
    }
}
