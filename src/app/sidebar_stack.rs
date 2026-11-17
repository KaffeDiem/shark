//! Stacked tasks in the sidebar: one block, newest on top, a 1px rail through the status
//! icons down to a base row. Membership is `stack::columns`; nothing about stacks is stored
//! but the trunks it needs at launch.
use super::*;
use crate::stack::Column;
use crate::sync::PrState;
use sidebar::sidebar_row;

/// A sidebar stack: its column and, per node, the task's index in `state.tasks`.
pub(super) struct SideStack {
    pub col: Column,
    pub tasks: Vec<Option<usize>>,
    /// The bottom task: the stack sits where it does in the list.
    pub anchor: usize,
}

impl SideStack {
    pub fn has(&self, ix: usize) -> bool {
        self.tasks.contains(&Some(ix))
    }

    /// Its tasks, bottom first.
    pub fn members(&self) -> impl Iterator<Item = usize> + '_ {
        self.tasks.iter().flatten().copied()
    }

    /// What a drag of it reads: "3 tasks", or the one task's title.
    fn drag_label(&self, tasks: &[Task]) -> SharedString {
        match self.members().count() {
            1 => tasks[self.anchor].display_title().into_owned().into(),
            n => format!("{n} tasks").into(),
        }
    }

    /// The node a sidebar row shows.
    fn node_of(&self, row: &SideRow) -> Option<usize> {
        match *row {
            SideRow::Task(ix) => self.tasks.iter().position(|t| *t == Some(ix)),
            SideRow::Pr(_, n) => (self.col.nodes.iter())
                .position(|x| x.ws.is_none() && x.pr.as_ref().is_some_and(|p| p.number == n)),
            _ => None,
        }
    }
}

/// What a stacked repo needs before it can land, in the git button's words: `(fact, warning)`.
pub(super) fn link_fact(r: &crate::sync::Repo) -> Option<(String, bool)> {
    if r.parent_merged() {
        let onto = r.parent.as_ref().map_or("", |p| p.base.as_str());
        return Some((format!("Parent merged · rebase onto {onto}"), false));
    }
    if r.parent_is(PrState::Closed).is_some() {
        return Some(("Parent PR closed".into(), true));
    }
    let parent = r.stacked()?;
    let l = r.local.as_ref().ok()?;
    (l.restack || l.base.1 > 0).then(|| (format!("Rebase onto {parent}"), false))
}

/// A task row's place in its stack.
pub(super) struct StackPlace {
    /// The bottom task's name and the drag's label: dragging any row drags the stack.
    pub drag: String,
    pub drag_label: SharedString,
    /// `stack 2 of 3, on Payment API`.
    pub label: String,
    /// Its PR merged: the status icon says so while it is idle.
    pub merged: bool,
}

/// Task row 44px, slim rows 24px, each 2px above the next (`side_row`); a rail button 32px,
/// 16 above the next (`rail_button`).
const TASK_H: f32 = 44.;
const SLIM_H: f32 = 24.;
const GAP: f32 = 2.;
const BUTTON: f32 = 32.;
const BUTTON_GAP: f32 = 16.;
/// Where the icon sits from a row's top: the 16px status icon, a slim row's 12px one.
const ICON_TOP: f32 = 6.;
const ICON: f32 = 16.;
const SLIM_ICON: f32 = 12.;
/// The rail stops this far short of each icon.
const CLEAR: f32 = 3.;
/// Space above and below a block outside the rail: padding, not margin, so the block's bounds
/// hold it and a drag preview moves it with the block.
fn pad(rail: bool) -> f32 {
    if rail { 0. } else { 6. }
}

impl Shark {
    /// The stacks the sidebar shows, from the saved bases, the cached trunks and the open
    /// PRs known. A repository filter shows only that repo's stacks; "No repository" none.
    pub(super) fn side_stacks(&self) -> Vec<SideStack> {
        match &self.sidebar_repo_filter {
            None => self.stacks_in(None),
            Some(home::RepoFilter::Repo(r)) => self.stacks_in(Some(r)),
            Some(_) => vec![],
        }
    }

    /// Every stack, whatever the filter: what moving and creating tasks keep together.
    fn all_stacks(&self) -> Vec<SideStack> {
        self.stacks_in(None)
    }

    fn stacks_in(&self, only: Option<&str>) -> Vec<SideStack> {
        let tasks = &self.state.tasks;
        let ix: HashMap<&str, usize> = (tasks.iter().enumerate())
            .map(|(i, t)| (t.name.as_str(), i))
            .collect();
        let grouped: HashSet<&str> = (self.state.sections.iter())
            .flat_map(|s| s.tasks.iter().map(String::as_str))
            .collect();
        // Sidebar order: the sections', then the ungrouped.
        let order = (self.state.sections.iter())
            .flat_map(|s| s.tasks.iter().filter_map(|n| ix.get(n.as_str()).copied()))
            .chain((0..tasks.len()).filter(|i| !grouped.contains(tasks[*i].name.as_str())));
        let listed: Vec<crate::stack::Listed> = order
            .map(|i| {
                let t = &tasks[i];
                let repos = (t.repos.iter())
                    .map(|r| {
                        (
                            r.name.as_str(),
                            self.state.origin_branch(t, r),
                            // A local parent's branch too, for a task cut from it unpushed.
                            self.state.on_origin(&r.name, &r.base),
                        )
                    })
                    .collect();
                (t.name.as_str(), repos)
            })
            .collect();
        let cols = crate::stack::columns(
            &listed,
            |r| self.state.trunks.get(r).map(String::as_str),
            |r| self.stack_prs.get(r).map_or(&[], |l| l.prs.as_slice()),
            only,
        );
        cols.into_iter()
            .filter_map(|col| {
                let at: Vec<Option<usize>> = (col.nodes.iter())
                    .map(|n| n.ws.as_deref().and_then(|w| ix.get(w).copied()))
                    .collect();
                let anchor = at.iter().flatten().copied().next()?;
                Some(SideStack {
                    col,
                    tasks: at,
                    anchor,
                })
            })
            .collect()
    }

    /// The tasks of `ws`'s stack, bottom first, whatever the filter; just `ws` outside one.
    pub(super) fn stack_mates(&self, ws: &str) -> Vec<String> {
        self.mates_in(ws, &self.all_stacks())
    }

    fn mates_in(&self, ws: &str, stacks: &[SideStack]) -> Vec<String> {
        let tasks = &self.state.tasks;
        (tasks.iter().position(|t| t.name == ws))
            .and_then(|ix| stacks.iter().find(|s| s.has(ix)))
            .map(|s| s.members().map(|i| tasks[i].name.clone()).collect())
            .unwrap_or_else(|| vec![ws.to_string()])
    }

    /// The section a task shows in: its stack's bottom task's.
    pub(super) fn shown_section(&self, ws: &str) -> Option<usize> {
        self.section_of(&self.mates_in(ws, &self.side_stacks())[0])
    }

    /// A task that joined a stack takes the stack's place: the old bottom's slot when it is
    /// the new bottom, and the stack's section. Adding a task on top moves nothing else.
    pub(super) fn settle_in_stack(&mut self, name: &str) {
        let mates = self.stack_mates(name);
        let Some(old) = mates.iter().find(|n| *n != name).cloned() else {
            return;
        };
        if mates[0] == name {
            self.state.drop_task(name, &old);
        }
        let section = self.section_of(&old);
        for m in &mates {
            self.state.move_to_section(m, section);
        }
    }

    /// Before `name` is removed: the next task of its stack takes its slot and section, so a
    /// stack whose bottom task goes stays where it was.
    pub(super) fn hand_over_slot(&mut self, name: &str) {
        let mates = self.stack_mates(name);
        if let [bottom, next, ..] = &mates[..]
            && bottom == name
        {
            self.state.drop_task(next, name);
            let section = self.section_of(name);
            for m in &mates[1..] {
                self.state.move_to_section(m, section);
            }
        }
    }

    /// Keeps what a probe found that stacks need: the trunks and the branches origin knows by
    /// another name (both saved when they change), and each repo's open PRs, unless older than
    /// the ones kept. A task follows its PR's base when GitHub retargeted it. Whether anything
    /// changed.
    pub(crate) fn learn_stacks(
        &mut self,
        repos: &[crate::sync::Repo],
        prs: &HashMap<String, crate::prs::OpenPrs>,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut trunk = false;
        for r in repos {
            let Ok(l) = &r.local else { continue };
            if !l.trunk.is_empty() && self.state.trunks.get(&r.name) != Some(&l.trunk) {
                self.state.trunks.insert(r.name.clone(), l.trunk.clone());
                trunk = true;
            }
            trunk |= self.learn_origin_branch(&r.name, &r.branch, &l.origin_branch);
        }
        if trunk {
            let _ = self.state.save();
        }
        let mut changed = trunk;
        for (repo, list) in prs {
            if !self.stack_prs_news(repo, list) {
                continue;
            }
            changed |= self.stack_prs.get(repo).is_none_or(|h| h.prs != list.prs);
            self.stack_prs.insert(repo.clone(), list.clone());
            // The disk cache may be older than what a task last followed.
            let Some(at) = list.at else { continue };
            let seen = (self.state.tasks.iter())
                .flat_map(|t| t.repos.iter().map(move |r| (t, r)))
                .filter(|(_, r)| r.name == *repo)
                .filter_map(|(t, r)| {
                    let pr = crate::stack::own_pr(&list.prs, self.state.origin_branch(t, r))?;
                    Some((t.name.clone(), r.name.clone(), pr.base_ref_name.clone()))
                })
                .collect();
            changed |= self.follow_prs(seen, at, cx);
        }
        changed
    }

    /// Keeps `on_origin`, the name a probe found origin has for `repo`'s `branch` (empty: not
    /// probed), when it is another name, and forgets one that no longer is. Whether it changed.
    fn learn_origin_branch(&mut self, repo: &str, branch: &str, on_origin: &str) -> bool {
        if on_origin.is_empty() || self.state.on_origin(repo, branch) == on_origin {
            return false;
        }
        let known = self.state.origin_branches.entry(repo.into()).or_default();
        match on_origin == branch {
            true => known.remove(branch),
            false => known.insert(branch.into(), on_origin.into()),
        };
        (self.state.origin_branches).retain(|_, known| !known.is_empty());
        true
    }

    /// Whether `list` is newer than `repo`'s open PRs kept; the disk cache only when none is.
    pub(crate) fn stack_prs_news(&self, repo: &str, list: &crate::prs::OpenPrs) -> bool {
        self.stack_prs.get(repo).is_none_or(|h| h.at < list.at)
    }

    /// Whether `repo`'s open PRs are due for listing: none from gh yet, or not lately.
    pub(crate) fn stack_prs_due(&self, repo: &str, now: std::time::Instant) -> bool {
        (self.stack_prs.get(repo).and_then(|l| l.at))
            .is_none_or(|t| now.duration_since(t) >= crate::sync::FETCH_EVERY)
    }

    /// GitHub said at `at` that each `(task, repo)`'s open PR targets `base`. An answer older
    /// than one already heard is dropped; a base GitHub changed is the task's from now on,
    /// as if set with "Only target". Whether a task moved.
    fn follow_prs(
        &mut self,
        seen: Vec<(String, String, String)>,
        at: std::time::Instant,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut moved: Vec<String> = vec![];
        for (task, repo, base) in seen {
            let key = (task.clone(), repo.clone());
            let before = match self.pr_seen.get(&key) {
                Some((t, _)) if *t >= at => continue,
                b => b.map(|(_, b)| b.clone()),
            };
            self.pr_seen.insert(key, (at, base.clone()));
            let Some(r) =
                (self.task_mut(&task)).and_then(|t| t.repos.iter_mut().find(|r| r.name == repo))
            else {
                continue;
            };
            if crate::stack::follows(&r.base, before.as_deref(), &base) {
                r.base = format!("origin/{base}");
                if !moved.contains(&task) {
                    moved.push(task);
                }
            }
        }
        if moved.is_empty() {
            return false;
        }
        let _ = self.state.save();
        for name in &moved {
            let Some(ws) = self.task(name).cloned() else {
                continue;
            };
            let _ = git::write_claude_md(&ws);
            if let Some(sync) = self.syncs.get(name).cloned() {
                sync.update(cx, |g, cx| g.set_task(ws, cx));
            }
            self.publish_task(name, cx);
            self.refresh_diff_if_active(name, cx);
        }
        true
    }

    /// The URL of the PR a PR row stands for.
    pub(super) fn stack_pr(stacks: &[SideStack], anchor: usize, number: u64) -> Option<String> {
        (stacks.iter().find(|s| s.anchor == anchor))?
            .col
            .nodes
            .iter()
            .find_map(|n| {
                n.pr.as_ref()
                    .filter(|p| p.number == number && n.ws.is_none())
            })
            .map(|p| p.url.clone())
    }

    /// The probed state of a stack task's repo in the stack.
    fn stack_repo<'a>(
        &'a self,
        st: &SideStack,
        ix: usize,
        cx: &'a App,
    ) -> Option<&'a crate::sync::Repo> {
        (self
            .overview_repos(&self.state.tasks[ix].name, cx)
            .into_iter()
            .flatten())
        .find(|r| r.name == st.col.repo)
    }

    /// A task row's place in `st` (its `node`), for its accessible label and drag.
    fn stack_place(&self, st: &SideStack, ix: usize, node: usize, cx: &App) -> StackPlace {
        let tasks = &self.state.tasks;
        let below = match node {
            0 => st.col.base.clone(),
            n => {
                let b = &st.col.nodes[n - 1];
                match (st.tasks[n - 1], &b.pr) {
                    (Some(t), _) => tasks[t].display_title().into_owned(),
                    (None, Some(p)) => format!("#{} {}", p.number, b.branch),
                    (None, None) => b.branch.clone(),
                }
            }
        };
        let merged = (self.stack_repo(st, ix, cx))
            .and_then(|r| r.pr())
            .is_some_and(|p| p.state == PrState::Merged);
        StackPlace {
            drag: tasks[st.anchor].name.clone(),
            drag_label: st.drag_label(tasks),
            label: format!("stack {} of {}, on {below}", node + 1, st.col.nodes.len()),
            merged,
        }
    }

    /// The task above a rail segment is behind or off its parent: the segment is dashed.
    fn link_broken(&self, st: &SideStack, node: usize, cx: &App) -> bool {
        (st.tasks[node])
            .and_then(|ix| self.stack_repo(st, ix, cx))
            .is_some_and(|r| link_fact(r).is_some())
    }

    /// A stack: its `rows` (from `side_rows`, newest on top), the base row, and the rail.
    pub(super) fn stack_block(
        &self,
        st: &SideStack,
        rows: &[SideRow],
        order: &[usize],
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (theme, rail) = (cx.theme(), self.rail);
        let tasks = &self.state.tasks;
        let indent = self.section_of(&tasks[st.anchor].name).map_or(0., |_| 32.);
        // Hovering a row previews what the open one shows: the rail below it, brighter.
        let lit =
            |name: &str| (st.tasks.iter()).position(|t| t.is_some_and(|i| tasks[i].name == name));
        let lit_at = (self.peek.row.as_deref()).and_then(lit).or_else(|| {
            (self.active.as_deref())
                .filter(|_| !self.home_shown())
                .and_then(lit)
        });
        let mut children: Vec<AnyElement> = vec![];
        // Per drawn node: its index (none for the base), and its icon's top and bottom.
        struct Mark {
            node: Option<usize>,
            top: f32,
            bottom: f32,
        }
        let mut marks: Vec<Mark> = vec![];
        let mut y = pad(rail);
        let mut mark = |node, icon: f32, height: f32, y: &mut f32| {
            let top = if rail { *y } else { *y + ICON_TOP };
            marks.push(Mark {
                node,
                top,
                bottom: top + icon,
            });
            *y += height;
        };
        for row in rows {
            let Some(node) = st.node_of(row) else {
                continue;
            };
            match *row {
                SideRow::Task(ix) if rail => {
                    let place = self.stack_place(st, ix, node, cx);
                    children.push(self.ws_row(ix, &tasks[ix], 0., order, Some(place), window, cx));
                    mark(Some(node), BUTTON, BUTTON + BUTTON_GAP, &mut y);
                }
                SideRow::Task(ix) if self.slim(ix) => {
                    children.push(self.slim_task_row(st, ix, node, indent, window, cx));
                    mark(Some(node), SLIM_ICON, SLIM_H + GAP, &mut y);
                }
                SideRow::Task(ix) => {
                    let place = self.stack_place(st, ix, node, cx);
                    let row = self.ws_row(ix, &tasks[ix], indent, order, Some(place), window, cx);
                    children.push(row);
                    mark(Some(node), ICON, TASK_H + GAP, &mut y);
                }
                SideRow::Pr(..) => {
                    children.push(self.slim_pr_row(st, node, indent, window, cx));
                    mark(Some(node), SLIM_ICON, SLIM_H + GAP, &mut y);
                }
                _ => {}
            }
        }
        if !rail {
            children.push(self.base_row(st, indent, cx));
            mark(None, SLIM_ICON, SLIM_H, &mut y);
        }
        // The icon column's centre: 16px in from the row (`side_row`'s margin, border, padding).
        let x = if rail { 72. / 2. } else { indent.max(8.) + 16. } - 0.5;
        let segments = marks.windows(2).filter_map(|w| {
            let (top, bottom) = (w[0].bottom + CLEAR, w[1].top - CLEAR);
            let node = w[0].node?;
            let color = match lit_at.is_some_and(|l| node <= l) {
                true => theme.foreground,
                false => theme.sidebar_foreground,
            };
            let line = div()
                .absolute()
                .left(px(x))
                .top(px(top))
                .w(px(1.))
                .h(px((bottom - top).max(0.)));
            Some(match self.link_broken(st, node, cx) {
                true => line.border_l_1().border_dashed().border_color(color),
                false => line.bg(color),
            })
        });
        let segments: Vec<_> = segments.collect();
        v_flex()
            .id(SharedString::from(format!(
                "stack-{}",
                tasks[st.anchor].name
            )))
            .test_support()
            .relative()
            .py(px(pad(rail)))
            .children(children)
            .children(segments)
            .into_any_element()
    }

    /// A stack's Backlog task gets a slim row, unless it is the open one.
    fn slim(&self, ix: usize) -> bool {
        let task = &self.state.tasks[ix];
        task.backlog && self.active.as_deref() != Some(&task.name)
    }

    /// Where `row` is in its stack's block (from its `rows`): top and height. `stack_block`
    /// stacks the rows the same way.
    fn row_span(&self, rows: &[SideRow], row: SideRow) -> (f32, f32) {
        // Each row's pitch and the gap below it.
        let pitch = |r: &SideRow| match *r {
            _ if self.rail => (BUTTON + BUTTON_GAP, BUTTON_GAP),
            SideRow::Task(ix) if !self.slim(ix) => (TASK_H + GAP, GAP),
            _ => (SLIM_H + GAP, GAP),
        };
        let above: f32 = (rows.iter())
            .take_while(|r| **r != row)
            .map(|r| pitch(r).0)
            .sum();
        let top = pad(self.rail) + above;
        let (p, g) = pitch(&row);
        (top, p - g)
    }

    /// Scrolls the sidebar so `rows[at]`, a row of a stack's block (the list's `child`), shows:
    /// a block taller than the list would otherwise always show its top. Whether it could.
    pub(super) fn scroll_to_stack_row(&self, rows: &[SideRow], at: usize, child: usize) -> bool {
        let stacks = self.side_stacks();
        let Some(st) = sidebar::stack_at(&stacks, &rows[at]) else {
            return false;
        };
        let Some(block) = self.sidebar_scroll.bounds_for_item(child) else {
            return false;
        };
        let same =
            |r: &&SideRow| sidebar::stack_at(&stacks, r).is_some_and(|s| s.anchor == st.anchor);
        let mine: Vec<SideRow> = rows.iter().filter(same).copied().collect();
        let (top, height) = self.row_span(&mine, rows[at]);
        let (top, view) = (block.top() + px(top), self.sidebar_scroll.bounds());
        let mut offset = self.sidebar_scroll.offset();
        if top + offset.y < view.top() {
            offset.y = view.top() - top;
        } else if top + px(height) + offset.y > view.bottom() {
            offset.y = view.bottom() - top - px(height);
        }
        self.sidebar_scroll.set_offset(offset);
        true
    }

    /// A Backlog task in a stack: a slim row that keeps the chain unbroken.
    fn slim_task_row(
        &self,
        st: &SideStack,
        ix: usize,
        node: usize,
        indent: f32,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let task = &self.state.tasks[ix];
        let place = self.stack_place(st, ix, node, cx);
        let title = task.display_title().into_owned();
        self.slim_row(indent, self.sel == SideRow::Task(ix), window, theme)
            .id(("stack-backlog", ix))
            .test_support()
            .role(gpui_kit::accesskit::Role::Button)
            .aria_label(format!("{title} · {} · Backlog", place.label))
            .child(slim_icon(Icon::new(IconName::CirclePause)))
            .child(div().flex_1().min_w_0().truncate().child(title))
            .map(|d| self.task_row_events(d, ix, task, cx))
            .map(|d| self.reorder_source(d, reorder::Item::Task(place.drag), place.drag_label))
            .when(
                self.peek.task.as_deref() == Some(&task.name) && !self.peek.home,
                |d| d.child(self.render_peek(task, window, cx)),
            )
            .into_any_element()
    }

    /// A PR in the chain without a task here, as a colleague's: click or enter opens it.
    fn slim_pr_row(
        &self,
        st: &SideStack,
        node: usize,
        indent: f32,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let n = &st.col.nodes[node];
        let Some(pr) = &n.pr else {
            return div().into_any_element();
        };
        let (anchor, number) = (st.anchor, pr.number);
        let status = match pr.status() {
            crate::prs::PrStatus::Open => "Open",
            crate::prs::PrStatus::Draft => "Draft",
            crate::prs::PrStatus::Merged => "Merged",
            crate::prs::PrStatus::Closed => "Closed",
        };
        let text = format!("#{number} {} · {status}", n.branch);
        let url = pr.url.clone();
        let drag = reorder::Item::Task(self.state.tasks[anchor].name.clone());
        let row = SideRow::Pr(anchor, number);
        self.slim_row(indent, self.sel == row, window, theme)
            .id(("stack-pr", number as usize))
            .test_support()
            .role(gpui_kit::accesskit::Role::Link)
            .aria_label(format!(
                "{text} · stack {} of {}",
                node + 1,
                st.col.nodes.len()
            ))
            .child(slim_icon(pr.status().icon(theme)))
            .child(div().flex_1().min_w_0().truncate().child(text))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |s, _, _, cx| {
                    s.sel = row;
                    cx.notify();
                }),
            )
            .on_click(move |_, _, cx| cx.open_url(&url))
            .map(|d| self.reorder_source(d, drag, st.drag_label(&self.state.tasks)))
            .into_any_element()
    }

    /// What the stack lands on, e.g. `main`; not a cursor stop.
    fn base_row(&self, st: &SideStack, indent: f32, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let several = st.members().any(|i| self.state.tasks[i].repos.len() > 1);
        let text = match several {
            true => format!("{} · {}", st.col.base, st.col.repo),
            false => st.col.base.clone(),
        };
        let tip: SharedString =
            format!("Tasks above merge into `{}`, bottom first.", st.col.base).into();
        sidebar_row(false, theme)
            .id(SharedString::from(format!(
                "stack-base-{}",
                self.state.tasks[st.anchor].name
            )))
            .test_support()
            .h(px(SLIM_H))
            .when(indent > 0., |d| d.ml(px(indent)))
            .text_xs()
            .aria_label(format!("Stack base, {}", st.col.base))
            .child(slim_icon(Icon::new(IconName::GitBranch)))
            .child(div().flex_1().min_w_0().truncate().child(text))
            .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
            .into_any_element()
    }

    fn slim_row(&self, indent: f32, selected: bool, window: &Window, theme: &Theme) -> Div {
        let focused = self.sidebar_focus.is_focused(window);
        sidebar_row(false, theme)
            .h(px(SLIM_H))
            .when(indent > 0., |d| d.ml(px(indent)))
            .text_xs()
            .map(|d| cursor_row(d, false, selected, focused, theme))
            .when(selected && focused, |d| d.text_color(theme.foreground))
    }
}

/// A 12px icon centred in the 16px status-icon column, so it sits on the rail.
fn slim_icon(icon: Icon) -> Div {
    div()
        .size(px(16.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(icon.with_size(px(12.)))
}
