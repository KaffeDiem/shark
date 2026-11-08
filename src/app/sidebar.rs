//! Agent navigation, task rows, manual task groups, and the sidebar cursor.
use super::*;

/// A task's status icon: a spinner while an agent runs, else by what it needs from you.
/// `idle` colours the dashed circle of a task that needs nothing.
pub(super) fn indicator(
    bucket: home::Bucket,
    idle: Hsla,
    size: Pixels,
    theme: &Theme,
) -> AnyElement {
    let (icon, color) = match bucket {
        home::Bucket::Running => {
            return Spinner::new()
                .icon(IconName::LoaderCircle)
                .color(theme.primary)
                .with_size(size)
                .into_any_element();
        }
        home::Bucket::Needs => (IconName::CircleAlert, theme.warning_foreground),
        home::Bucket::Unread => (IconName::CircleCheck, theme.success_foreground),
        home::Bucket::Idle => (IconName::CircleDashed, idle),
    };
    Icon::new(icon)
        .with_size(size)
        .text_color(color)
        .into_any_element()
}

pub(super) fn task_indicator(
    running: usize,
    attention: &Attn,
    foreground: Hsla,
    size: Pixels,
    theme: &Theme,
) -> AnyElement {
    indicator(
        home::bucket(running, attention, None).0,
        foreground,
        size,
        theme,
    )
}

/// Icons a section can have, by Lucide name (what `Section::icon` stores).
pub(super) const SECTION_ICONS: &[(&str, IconName)] = &[
    ("folder", IconName::Folder),
    ("bot", IconName::Bot),
    ("star", IconName::Star),
    ("briefcase", IconName::Briefcase),
    ("bug", IconName::Bug),
    ("rocket", IconName::Rocket),
    ("flame", IconName::Flame),
    ("zap", IconName::Zap),
    ("heart", IconName::Heart),
    ("bookmark", IconName::Bookmark),
    ("flag", IconName::Flag),
    ("tag", IconName::Tag),
    ("inbox", IconName::Inbox),
    ("clock", IconName::Clock),
    ("calendar", IconName::Calendar),
    ("users", IconName::Users),
    ("code", IconName::Code),
    ("wrench", IconName::Wrench),
    ("flask-conical", IconName::FlaskConical),
    ("lightbulb", IconName::Lightbulb),
    ("layers", IconName::Layers),
    ("box", IconName::Box),
    ("globe", IconName::Globe),
    ("shield", IconName::Shield),
    ("sparkles", IconName::Sparkles),
    ("target", IconName::Target),
    ("palette", IconName::Palette),
    ("coffee", IconName::Coffee),
    ("book-open", IconName::BookOpen),
];

pub(super) fn section_icon(name: &str) -> IconName {
    SECTION_ICONS
        .iter()
        .find(|(n, _)| *n == name)
        .map_or(IconName::Folder, |(_, i)| *i)
}

/// "Move to task group" in a task's right-click menu; ends with "New task group", which moves it there.
pub(super) fn move_submenu(this: &Shark, ws: &str, cx: &Context<Shark>) -> Entry {
    let current = this.section_of(ws);
    let mut entries = vec![];
    for (i, sec) in this
        .state
        .sections
        .iter()
        .enumerate()
        .filter(|(_, s)| s.agent.is_none())
    {
        let ws = ws.to_string();
        let item = menu::item(
            section_icon(&sec.icon),
            sec.name.clone(),
            on_shark(cx, move |s, _, cx| s.move_to_section(&ws, Some(i), cx)),
        );
        entries.push(item.checked(current == Some(i)));
    }
    if current.is_some() {
        let ws = ws.to_string();
        entries.push(menu::item(
            IconName::FolderOutput,
            "No task group",
            on_shark(cx, move |s, _, cx| s.move_to_section(&ws, None, cx)),
        ));
    }
    if !entries.is_empty() {
        entries.push(Entry::Sep);
    }
    let ws = ws.to_string();
    entries.push(menu::item(
        IconName::FolderPlus,
        "New task group",
        on_shark(cx, move |s, w, cx| {
            s.start_naming(None, Some(ws.clone()), w, cx)
        }),
    ));
    menu::sub(IconName::FolderInput, "Move to task group", entries)
}

/// Right-click menu of a section header.
pub(super) fn section_menu(_this: &Shark, i: usize, cx: &Context<Shark>) -> Vec<Entry> {
    vec![
        menu::item(
            IconName::Pencil,
            "Rename",
            on_shark(cx, move |s, w, cx| s.start_naming(Some(i), None, w, cx)),
        ),
        menu::item(
            IconName::Shapes,
            "Change icon…",
            on_shark(cx, move |s, w, cx| s.pick_icon(i, w, cx)),
        ),
        menu::item(
            IconName::FolderPlus,
            "New task group",
            on_shark(cx, |s, w, cx| s.start_naming(None, None, w, cx)),
        ),
        Entry::Sep,
        menu::item(
            IconName::Trash,
            "Delete task group",
            on_shark(cx, move |s, w, cx| s.delete_section(i, w, cx)),
        ),
    ]
}

/// Cursor state taken by [`Shark::cursors`]: sidebar row, its task's name, home row.
pub(super) struct Cursors {
    side: Option<usize>,
    ws: Option<String>,
    home: Option<usize>,
}

/// A sidebar row the cursor can sit on.
#[derive(PartialEq, Clone, Copy, Debug)]
pub(super) enum SideRow {
    /// Index into `state.agents`.
    Agent(usize),
    /// Index into `state.tasks`.
    Task(usize),
}

pub(super) fn drag_chip(label: SharedString, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .px_2()
        .py_1()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.secondary)
        .text_sm()
        .text_color(theme.foreground)
        .font_features(inter_features())
        .child(label)
}

/// 16px icons; the rail is 32px icon buttons, 16 apart.
const ISZ: Pixels = px(16.);

/// A row of the sidebar's lists.
fn sidebar_row(rail: bool, theme: &Theme) -> Div {
    side_row(theme)
        .text_color(theme.sidebar_foreground)
        .when(rail, rail_button)
}

/// A sidebar row in the rail: Paper's 32px icon button, centered, 16 above the next.
pub(super) fn rail_button<E: Styled>(e: E) -> E {
    e.size(px(32.))
        .mx_auto()
        .mb(px(16.))
        .px_0()
        .justify_center()
        .rounded(px(8.))
}

impl Shark {
    pub(super) fn section_of(&self, ws: &str) -> Option<usize> {
        self.state
            .sections
            .iter()
            .position(|s| s.agent.is_none() && s.tasks.iter().any(|w| w == ws))
    }

    /// Agent children come from their task snapshots, never from manual groups.
    pub(super) fn side_rows(&self, unfold: bool) -> Vec<SideRow> {
        let mut rows = vec![];
        for (i, agent) in self.state.agents.iter().enumerate() {
            rows.push(SideRow::Agent(i));
            if unfold || !self.collapsed_agents.contains(&agent.slug) {
                rows.extend(
                    self.state
                        .tasks
                        .iter()
                        .enumerate()
                        .filter(|(_, task)| {
                            task.agent.as_ref().is_some_and(|a| a.slug == agent.slug)
                        })
                        .map(|(i, _)| SideRow::Task(i)),
                );
            }
        }
        rows.extend(
            self.state
                .tasks
                .iter()
                .enumerate()
                .filter(|(_, task)| {
                    !task
                        .agent
                        .as_ref()
                        .is_some_and(|a| self.state.agents.iter().any(|agent| agent.slug == a.slug))
                })
                .map(|(i, _)| SideRow::Task(i)),
        );
        rows
    }

    fn cursor_agent(&self) -> Option<usize> {
        match self.sel {
            SideRow::Agent(i) => self.state.agents.get(i).map(|_| i),
            SideRow::Task(i) => {
                let slug = &self.state.tasks.get(i)?.agent.as_ref()?.slug;
                self.state.agents.iter().position(|a| &a.slug == slug)
            }
        }
    }

    pub(super) fn set_agent_collapsed(
        &mut self,
        i: usize,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(agent) = self.state.agents.get(i) else {
            return;
        };
        if collapsed {
            self.collapsed_agents.insert(agent.slug.clone());
        } else {
            self.collapsed_agents.remove(&agent.slug);
        }
        self.fix_sel();
        cx.notify();
    }

    /// Sidebar and home cursors before a task is removed; see [`Self::restore_cursors`].
    pub(super) fn cursors(&self, cx: &App) -> Cursors {
        Cursors {
            side: self.side_rows(false).iter().position(|r| *r == self.sel),
            // The sidebar cursor follows its task, not its row number.
            ws: match self.sel {
                SideRow::Task(ix) => self.state.tasks.get(ix).map(|w| w.name.clone()),
                SideRow::Agent(_) => None,
            },
            home: self.home_at(&self.home_rows(cx)),
        }
    }

    /// After `name` is removed: a cursor on its row stays at the same height, on whatever moved up into it.
    pub(super) fn restore_cursors(&mut self, name: &str, before: Cursors, cx: &App) {
        let rows = self.side_rows(false);
        let moved = (before.ws.as_deref().filter(|c| *c != name))
            .and_then(|c| self.state.tasks.iter().position(|w| w.name == c));
        self.sel = match (self.sel, moved) {
            (SideRow::Agent(i), _) if i < self.state.agents.len() => SideRow::Agent(i),
            (_, Some(ix)) => SideRow::Task(ix),
            _ => before
                .side
                .and_then(|p| rows.get(p.min(rows.len().saturating_sub(1))))
                .copied()
                .unwrap_or(SideRow::Task(0)),
        };
        if self.home.sel.as_deref() == Some(name) {
            let rows = self.home_rows(cx);
            let at = before.home.unwrap_or(0).min(rows.len().saturating_sub(1));
            self.home.sel = rows.get(at).map(|r| self.state.tasks[r.ix].name.clone());
        }
    }

    /// Tasks in sidebar order, folded ones only with `unfold`; cmd-N opens the Nth visible.
    pub(crate) fn ws_order(&self, unfold: bool) -> Vec<usize> {
        self.side_rows(unfold)
            .into_iter()
            .filter_map(|r| match r {
                SideRow::Task(ix) => Some(ix),
                SideRow::Agent(_) => None,
            })
            .collect()
    }

    /// Keeps the cursor on a visible row; a task hidden by collapsing hands it to its header.
    fn fix_sel(&mut self) {
        let rows = self.side_rows(false);
        if rows.contains(&self.sel) {
            return;
        }
        self.sel = (self.cursor_agent())
            .map(SideRow::Agent)
            .or(rows.first().copied())
            .unwrap_or(SideRow::Task(0));
    }

    fn move_sel(&mut self, nav: Nav, cx: &mut Context<Self>) {
        let rows = self.side_rows(false);
        if rows.is_empty() {
            return;
        }
        let at = rows.iter().position(|r| *r == self.sel).unwrap_or(0);
        let to = nav.to(at, rows.len(), 0);
        self.sel = rows[to];
        // Child 0 of the scrolled list is the Agents header.
        let other = matches!(self.sel, SideRow::Task(i) if self.state.tasks[i].agent.as_ref().is_none_or(|a| !self.state.agents.iter().any(|agent| agent.slug == a.slug)));
        self.sidebar_scroll
            .scroll_to_item(to + 1 + usize::from(other));
        cx.notify();
    }

    /// Group actions follow the same visible task as rename and archive actions.
    fn target_section(&self, window: &Window) -> Option<usize> {
        self.section_of(&self.target(window)?.name)
    }

    pub(super) fn with_section(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Self, usize, &mut Window, &mut Context<Self>),
    ) {
        match self.target_section(window) {
            Some(i) => f(self, i, window, cx),
            None => notify_err(
                window,
                cx,
                "Put the sidebar cursor on a task in a group first",
            ),
        }
    }

    /// `h` folds the cursor's agent and parks the cursor on its header.
    fn collapse_at_cursor(&mut self, cx: &mut Context<Self>) {
        if let Some(i) = self.cursor_agent() {
            self.sel = SideRow::Agent(i);
            self.set_agent_collapsed(i, true, cx);
            cx.notify();
        }
    }

    pub(super) fn move_to_section(&mut self, ws: &str, to: Option<usize>, cx: &mut Context<Self>) {
        if to.is_some_and(|i| self.state.sections.get(i).is_none_or(|s| s.agent.is_some())) {
            return;
        }
        if self.section_of(ws) == to {
            return;
        }
        self.state.move_to_section(ws, to);
        let _ = self.state.save();
        self.record_interaction(ws, cx);
        self.fix_sel();
        cx.notify();
    }

    /// Dragging changes order only, never group or agent membership.
    pub(super) fn can_reorder_tasks(&self, from: &str, to: &str) -> bool {
        let (Some(from_task), Some(to_task)) = (self.task(from), self.task(to)) else {
            return false;
        };
        from_task.agent.as_ref().map(|a| &a.slug) == to_task.agent.as_ref().map(|a| &a.slug)
            && self.section_of(from) == self.section_of(to)
    }

    pub(super) fn drop_task(&mut self, ws: &str, target: &str, cx: &mut Context<Self>) {
        if !self.can_reorder_tasks(ws, target) {
            return;
        }
        // The task under the cursor, so the reorder that shifts indices can put it back.
        let cursor = match self.sel {
            SideRow::Task(ix) => self.state.tasks.get(ix).map(|w| w.name.clone()),
            SideRow::Agent(_) => None,
        };
        if let (Some(from), Some(to)) = (
            self.state.tasks.iter().position(|t| t.name == ws),
            self.state.tasks.iter().position(|t| t.name == target),
        ) {
            move_item(&mut self.state.tasks, from, to);
        }
        if let Some(i) = self.section_of(ws) {
            let tasks = &mut self.state.sections[i].tasks;
            if let (Some(from), Some(to)) = (
                tasks.iter().position(|n| n == ws),
                tasks.iter().position(|n| n == target),
            ) {
                move_item(tasks, from, to);
            }
        }
        if let Some(ix) = cursor.and_then(|n| self.state.tasks.iter().position(|w| w.name == n)) {
            self.sel = SideRow::Task(ix);
        }
        let _ = self.state.save();
        self.fix_sel();
        cx.notify();
    }

    pub(super) fn delete_section(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.sections.get(i).is_none_or(|s| s.agent.is_some()) {
            return;
        }
        let s = self.state.sections.remove(i);
        let _ = self.state.save();
        self.fix_sel();
        window.push_notification(
            format!("Deleted task group {} (its tasks stay)", s.name),
            cx,
        );
        cx.notify();
    }

    /// Manual groups are edited in a dialog; they never assign an agent.
    pub(super) fn start_naming(
        &mut self,
        rename: Option<usize>,
        then_move: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if rename.is_some_and(|i| self.state.sections.get(i).is_none_or(|s| s.agent.is_some())) {
            return;
        }
        let original = rename
            .and_then(|i| self.state.sections.get(i))
            .map(|s| s.name.clone());
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Task group name")
                .default_value(original.clone().unwrap_or_default())
        });
        let field = input.clone();
        let shark = cx.entity().downgrade();
        window.open_dialog(cx, move |d, _, _| {
            let (input, shark, original, then_move) = (
                input.clone(),
                shark.clone(),
                original.clone(),
                then_move.clone(),
            );
            crate::ui::dialog(d)
                .title(if original.is_some() {
                    "Rename task group"
                } else {
                    "New task group"
                })
                .w(px(440.))
                .child(Input::new(&input))
                .on_ok(move |_, window, cx| {
                    let name = input.read(cx).value().trim().to_string();
                    if name.is_empty() {
                        return false;
                    }
                    shark
                        .update(cx, |s, cx| {
                            // Resolve the original name again if groups changed while the dialog was open.
                            let index = original.as_ref().and_then(|name| {
                                s.state
                                    .sections
                                    .iter()
                                    .position(|g| g.agent.is_none() && &g.name == name)
                            });
                            if original.is_some() && index.is_none() {
                                return true;
                            }
                            if s.state
                                .sections
                                .iter()
                                .enumerate()
                                .any(|(i, g)| g.name == name && Some(i) != index)
                            {
                                notify_err(
                                    window,
                                    cx,
                                    format!("There is already a task group {name}"),
                                );
                                return false;
                            }
                            let index = if let Some(i) = index {
                                s.state.sections[i].name = name;
                                i
                            } else {
                                s.state.sections.push(Section {
                                    agent: None,
                                    name,
                                    icon: "folder".into(),
                                    collapsed: false,
                                    tasks: vec![],
                                });
                                s.state.sections.len() - 1
                            };
                            if let Some(task) = &then_move {
                                s.move_to_section(task, Some(index), cx);
                            }
                            let _ = s.state.save();
                            s.sidebar_focus.focus(window, cx);
                            cx.notify();
                            true
                        })
                        .unwrap_or(true)
                })
        });
        field.update(cx, |i, cx| {
            i.focus(window, cx);
            i.select_all(window, cx);
        });
    }

    pub(super) fn pick_icon(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(section) = self.state.sections.get(i) else {
            return;
        };
        if section.agent.is_some() {
            return;
        }
        self.pick_named_icon(
            section.icon.clone(),
            move |s, icon, cx| {
                if let Some(section) = s.state.sections.get_mut(i) {
                    section.icon = icon;
                    let _ = s.state.save();
                    cx.notify();
                }
            },
            window,
            cx,
        );
    }

    pub(super) fn pick_agent_icon(
        &mut self,
        slug: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(a) = self.state.agents.iter().find(|a| a.slug == slug) else {
            return;
        };
        let slug = slug.to_string();
        self.pick_named_icon(
            a.icon.clone(),
            move |s, icon, cx| {
                s.update_agent(&slug, |a| a.icon = icon, cx);
            },
            window,
            cx,
        );
    }

    fn pick_named_icon(
        &mut self,
        current: String,
        set: impl Fn(&mut Self, String, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let items = SECTION_ICONS
            .iter()
            .map(|(name, icon)| {
                CommandItem::new()
                    .label(name.replace('-', " "))
                    .icon(*icon)
                    .checked(*name == current)
            })
            .collect();
        let selected = SECTION_ICONS
            .iter()
            .position(|(n, _)| *n == current)
            .unwrap_or(0);
        open_menu(
            cx.entity().downgrade(),
            "Pick an icon…",
            items,
            selected,
            move |s, row, _, cx| set(s, SECTION_ICONS[row].0.into(), cx),
            window,
            cx,
        );
    }

    /// Sections, then "No task group" when it is in one, then "New task group".
    pub(super) fn pick_section(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.with_target(window, cx, |s, ws, window, cx| {
            let ws = ws.name;
            let current = s.section_of(&ws);
            let indices: Vec<_> = s
                .state
                .sections
                .iter()
                .enumerate()
                .filter(|(_, g)| g.agent.is_none())
                .map(|(i, _)| i)
                .collect();
            let n = indices.len();
            let mut items: Vec<CommandItem> = (s.state.sections)
                .iter()
                .enumerate()
                .filter(|(_, s)| s.agent.is_none())
                .map(|(i, s)| {
                    CommandItem::new()
                        .label(s.name.clone())
                        .icon(section_icon(&s.icon))
                        .checked(current == Some(i))
                })
                .collect();
            if current.is_some() {
                items.push(
                    CommandItem::new()
                        .label("No task group")
                        .icon(IconName::FolderOutput),
                );
            }
            items.push(
                CommandItem::new()
                    .label("New task group")
                    .icon(IconName::FolderPlus),
            );
            let last = items.len() - 1;
            open_menu(
                cx.entity().downgrade(),
                "Move to task group…",
                items,
                current
                    .and_then(|i| indices.iter().position(|j| *j == i))
                    .unwrap_or(0),
                move |s, row, window, cx| match row {
                    _ if row == last => s.start_naming(None, Some(ws.clone()), window, cx),
                    _ if row < n => s.move_to_section(&ws, Some(indices[row]), cx),
                    _ => s.move_to_section(&ws, None, cx),
                },
                window,
                cx,
            );
        })
    }

    /// A task's row; `order` is the visible tasks, whose first nine have a cmd-N.
    fn ws_row(
        &self,
        ix: usize,
        w: &Task,
        section: Option<usize>,
        order: &[usize],
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (theme, rail) = (cx.theme(), self.rail);
        let focused = self.sidebar_focus.is_focused(window);
        let m = window.modifiers();
        let cmd_held = m.platform && m.number_of_modifiers() == 1;
        let running = self.running_chats(&w.name, cx);
        let selected = self.sel == SideRow::Task(ix);
        let active = !self.home_shown() && self.active.as_ref() == Some(&w.name);
        let bright = active || (selected && focused);
        let foreground = if bright {
            theme.foreground
        } else {
            theme.sidebar_foreground
        };
        let attn = self.attention_of(&w.name, cx);
        let needs = match &attn {
            Attn::Needs(step) => Some(step.clone()),
            _ => None,
        };
        let nth = order.iter().position(|&o| o == ix).filter(|&n| n < 9);
        let ws = w.clone();
        sidebar_row(rail, theme)
            .id(ix)
            .test_support()
            // One line; a task waiting on you adds what it waits on beneath, as Devin's
            // sessions do. Agent tasks indent one step, so their status icon lines up with the
            // agent's name.
            .when(!rail && needs.is_some(), |d| d.h(px(44.)))
            .when(section.is_some() && !rail, |d| d.ml(px(32.)))
            .map(|d| cursor_row(d, active, selected, focused, theme))
            .text_color(foreground)
            // The cursor moves on press, not release: a held or dragged row is the one ringed.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.sel = SideRow::Task(ix);
                    cx.notify();
                }),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.open_task(ix, window, cx)))
            // Right-click moves the cursor too, so it's clear which row the menu is for.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &MouseDownEvent, w, cx| {
                    this.sel = SideRow::Task(ix);
                    let entries = ws_menu(this, &ws, cx);
                    this.show_menu(e.position, entries, w, cx);
                }),
            )
            .map(|d| {
                self.reorder_source(
                    d,
                    reorder::Item::Task(w.name.clone()),
                    w.display_title().to_string().into(),
                )
            })
            .child(match nth {
                Some(n) if cmd_held => kbd(&format!("cmd-{}", n + 1)).into_any_element(),
                _ => task_indicator(running, &attn, foreground, ISZ, theme),
            })
            .when(!rail, |d| {
                d.child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .when(active, |d| d.font_weight(FontWeight::MEDIUM))
                                .child(w.display_title().to_string()),
                        )
                        .children(needs.map(|step| {
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(theme.warning_foreground)
                                .child(step)
                        })),
                )
            })
            .when(rail, |d| {
                let name: SharedString = w.display_title().to_string().into();
                d.tooltip(move |w, cx| Tooltip::new(name.clone()).build(w, cx))
            })
            // Holding cmd in the rail names every task beside its cmd-N key.
            .when(rail && cmd_held, |d| {
                d.relative().child(
                    div().absolute().top_0().left_full().child(deferred(
                        anchored().child(
                            div()
                                .ml_2()
                                .h(px(28.))
                                .px_2()
                                .flex()
                                .items_center()
                                .whitespace_nowrap()
                                .rounded(theme.radius)
                                .border_1()
                                .border_color(theme.border)
                                .bg(theme.popover)
                                .text_color(theme.popover_foreground)
                                .text_sm()
                                .shadow_md()
                                .child(w.display_title().to_string()),
                        ),
                    )),
                )
            })
            .into_any_element()
    }

    fn agent_row(&self, i: usize, window: &Window, cx: &Context<Self>) -> AnyElement {
        let agent = &self.state.agents[i];
        let (theme, rail) = (cx.theme(), self.rail);
        let focused = self.sidebar_focus.is_focused(window);
        let selected = self.sel == SideRow::Agent(i);
        let active = self.home_shown()
            && matches!(&self.home.destination,
            home::Destination::Agent(slug) if slug == &agent.slug);
        let collapsed = self.collapsed_agents.contains(&agent.slug);
        let tasks: Vec<_> = self
            .state
            .tasks
            .iter()
            .filter(|t| t.agent.as_ref().is_some_and(|a| a.slug == agent.slug))
            .collect();
        let needs = tasks
            .iter()
            .filter(|t| matches!(self.attention_of(&t.name, cx), Attn::Needs(_)))
            .count();
        let running = tasks
            .iter()
            .filter(|t| self.running_chats(&t.name, cx) > 0)
            .count();
        let slug = agent.slug.clone();
        let (menu_slug, new_slug) = (slug.clone(), slug.clone());
        let tip: SharedString = format!("{} · {} tasks", agent.name, tasks.len()).into();
        let group = SharedString::from(format!("agent-row-{i}"));
        let bright = active || (selected && focused);
        let quiet = if bright {
            theme.foreground
        } else {
            theme.sidebar_foreground
        };
        // One fact on the right, the most urgent: tasks waiting on you, else running ones,
        // else (folded) how many it has.
        let badge = match (needs, running) {
            (1.., _) => Some(
                h_flex()
                    .gap_1()
                    .text_xs()
                    .text_color(theme.warning_foreground)
                    .child(Icon::new(IconName::CircleAlert).with_size(px(12.)))
                    .child(needs.to_string())
                    .into_any_element(),
            ),
            (_, 1..) => Some(
                h_flex()
                    .gap_1()
                    .text_xs()
                    .child(Spinner::new().with_size(px(12.)).color(theme.primary))
                    .child(running.to_string())
                    .into_any_element(),
            ),
            _ if collapsed && !tasks.is_empty() => Some(
                div()
                    .text_xs()
                    .child(tasks.len().to_string())
                    .into_any_element(),
            ),
            _ => None,
        };
        sidebar_row(rail, theme)
            .id(("agent", i))
            .group(group.clone())
            .role(gpui_kit::accesskit::Role::Button)
            .aria_label(format!(
                "Open {} agent, {} tasks{}",
                agent.name,
                tasks.len(),
                match needs {
                    0 => String::new(),
                    n => format!(", {n} need you"),
                }
            ))
            .test_support()
            .map(|d| cursor_row(d, active, selected, focused, theme))
            .when(selected && focused, |d| d.text_color(theme.foreground))
            .on_click(cx.listener(move |this, _, w, cx| {
                this.sel = SideRow::Agent(i);
                this.open_agent(&slug, w, cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &MouseDownEvent, w, cx| {
                    this.sel = SideRow::Agent(i);
                    let entries = this.agent_menu(&menu_slug, cx);
                    this.show_menu(e.position, entries, w, cx);
                }),
            )
            .child(
                Icon::new(section_icon(&agent.icon))
                    .with_size(ISZ)
                    .when(needs > 0, |i| i.text_color(theme.warning_foreground)),
            )
            .when(!rail, |d| {
                d.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .child(agent.name.clone()),
                )
                // Keep hover controls before the badge so its spacing and position stay fixed.
                .child(
                    div()
                        .invisible()
                        .group_hover(group.clone(), |s| s.visible())
                        .child(
                            Button::new(("new-agent-task", i))
                                .accessibility_label(format!("New task for {}", agent.name))
                                .ghost()
                                .xsmall()
                                .text_color(quiet)
                                .icon(IconName::Plus)
                                .tooltip("New task")
                                .on_click(cx.listener(move |this, _, w, cx| {
                                    cx.stop_propagation();
                                    this.prefill_agent_task(&new_slug, w, cx);
                                })),
                        ),
                )
                .children(badge)
                .child(
                    Button::new(("collapse-agent", i))
                        .accessibility_label(format!(
                            "{} {} tasks",
                            if collapsed { "Expand" } else { "Collapse" },
                            agent.name
                        ))
                        .ghost()
                        .xsmall()
                        // The button's own foreground would outshine the label it belongs to.
                        .text_color(quiet)
                        .icon(if collapsed {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .tooltip(if collapsed {
                            "Expand tasks"
                        } else {
                            "Collapse tasks"
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.set_agent_collapsed(i, !collapsed, cx);
                        })),
                )
            })
            .when(rail, |d| {
                d.tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
            })
            .into_any_element()
    }

    pub(super) fn render_sidebar(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let rail = self.rail;
        let header = |t: &'static str| {
            side_header(theme, t)
                .text_color(theme.sidebar_foreground)
                .when(rail, |d| d.hidden())
        };
        let hint = |t: &'static str| {
            div()
                .when(rail, |d| d.hidden())
                .px_4()
                .text_xs()
                .text_color(theme.sidebar_foreground)
                .child(t)
        };
        let order = self.ws_order(false);
        let accent = theme.sidebar_accent;
        let mut items: Vec<AnyElement> = vec![];
        let mut other_header = false;
        for row in self.side_rows(false) {
            match row {
                SideRow::Agent(i) => items.push(self.agent_row(i, window, cx)),
                SideRow::Task(i) => {
                    let task = &self.state.tasks[i];
                    let agent = task.agent.as_ref().and_then(|a| {
                        self.state
                            .agents
                            .iter()
                            .position(|agent| agent.slug == a.slug)
                    });
                    if agent.is_none() && !other_header {
                        items.push(header("Unassigned").into_any_element());
                        other_header = true;
                    }
                    items.push(self.reorder_item(
                        self.ws_row(i, task, agent, &order, window, cx),
                        reorder::Item::Task(task.name.clone()),
                    ));
                }
            }
        }
        let new_task_menu = |add_repo: bool| {
            cx.listener(move |this, e: &MouseDownEvent, w, cx| {
                let mut entries = vec![menu::item(
                    IconName::Plus,
                    "New task",
                    on_shark(cx, |s, w, cx| s.new_empty_task(w, cx)),
                )];
                if add_repo {
                    entries.push(menu::item(
                        IconName::FolderGit2,
                        "Add repo…",
                        on_shark(cx, |s, w, cx| s.add_repo(w, cx)),
                    ));
                }
                this.show_menu(e.position, entries, w, cx);
            })
        };
        // "+" sits on the label's line, right-aligned with the agent rows' chevrons.
        let agents_header = header("Agents")
            .id("agents-header")
            .test_support()
            .when(!rail, |d| d.flex())
            .justify_between()
            .items_center()
            .child(
                Button::new("new-agent")
                    .accessibility_label("New agent")
                    .ghost()
                    .xsmall()
                    .text_color(theme.sidebar_foreground)
                    .icon(IconName::Plus)
                    .tooltip("New agent")
                    .on_click(cx.listener(|this, _, w, cx| this.new_agent_dialog(w, cx))),
            );
        let add_repo_row = sidebar_row(rail, theme)
            .id("add-repo-row")
            .hover(|d| d.bg(side_hover(theme)))
            .child(Icon::new(IconName::FolderPlus).with_size(ISZ))
            .when(!rail, |d| d.child(div().flex_1().child("Add repo…")))
            .when(rail, |d| {
                d.tooltip(|w, cx| Tooltip::new("Add repo").build(w, cx))
            })
            .on_click(cx.listener(|this, _, w, cx| this.add_repo(w, cx)));
        v_flex()
            .id("sidebar")
            // Folders dragged in from Finder are added as repos.
            .drag_over::<ExternalPaths>(move |d, _, _, _| d.bg(accent))
            .on_drop(cx.listener(|this, p: &ExternalPaths, w, cx| {
                this.add_repo_paths(p.paths(), w, cx);
            }))
            .key_context("Sidebar")
            .track_focus(&self.sidebar_focus)
            // The rail clears the traffic lights (x 9..61), so the content island never sits under them.
            // Expanded, it fills its resizable panel.
            .map(|d| match rail {
                true => d.w(px(72.)),
                false => d.w_full(),
            })
            .h_full()
            .flex_none()
            .map(|d| nav_actions(d, false, cx, Shark::move_sel))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| match this.sel {
                SideRow::Task(ix) => this.open_task(ix, window, cx),
                SideRow::Agent(i) => {
                    if let Some(slug) = this.state.agents.get(i).map(|a| a.slug.clone()) {
                        this.open_agent(&slug, window, cx);
                    }
                }
            }))
            .on_action(cx.listener(|this, _: &CollapseDir, _, cx| this.collapse_at_cursor(cx)))
            .on_action(cx.listener(|this, _: &ExpandDir, _, cx| {
                if let Some(i) = this.cursor_agent() {
                    this.set_agent_collapsed(i, false, cx)
                }
            }))
            .child(
                h_flex()
                    .h(px(44.))
                    .flex_none()
                    .child(
                        div()
                            .w(px(if rail { 72. } else { 80. }))
                            .h_full()
                            .flex_none()
                            .id("traffic-lights-empty")
                            .map(|d| self.draggable_chrome("traffic-lights-empty", d)),
                    )
                    .when(!rail, |d| d.child(self.render_sidebar_toggle(cx)))
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .id("sidebar-header-empty")
                            .map(|d| self.draggable_chrome("sidebar-header-empty", d)),
                    ),
            )
            .child(
                h_flex()
                    .flex_none()
                    .when(rail, |d| d.pt(px(8.)))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .child(self.render_home_row(rail, window, cx)),
                    )
                    .when(!rail, |d| {
                        d.child(
                            h_flex()
                                .flex_none()
                                .gap(px(4.))
                                .mr(px(8.))
                                .child(self.render_commands(cx))
                                .child(self.render_new_task(cx)),
                        )
                    }),
            )
            .child(
                // Children are indexed by `move_sel`: the header, then one per `side_rows` row.
                v_flex()
                    .id("sidebar-rows")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.sidebar_scroll)
                    .child(agents_header)
                    .children(items)
                    .when(
                        self.state.tasks.is_empty() && self.state.agents.is_empty(),
                        |d| d.child(hint("cmd-n to create")),
                    )
                    .when(self.state.repos.is_empty(), |d| d.child(add_repo_row))
                    // The empty space below takes a right-click too.
                    .child(
                        div()
                            .id("sidebar-rest")
                            .flex_1()
                            .min_h(px(24.))
                            .on_mouse_down(MouseButton::Right, new_task_menu(true)),
                    ),
            )
            .child(self.render_usage_footer(cx))
            .test_support()
    }
}
