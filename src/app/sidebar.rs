//! The sidebar: Home, sections and their tasks, ungrouped tasks, the cursor.
use super::*;
use gpui_kit::component::Selectable as _;

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
        home::Bucket::Backlog => (IconName::CirclePause, idle),
    };
    Icon::new(icon)
        .with_size(size)
        .text_color(color)
        .into_any_element()
}

pub(super) fn task_indicator(
    backlog: bool,
    running: usize,
    attention: &Attn,
    foreground: Hsla,
    size: Pixels,
    theme: &Theme,
) -> AnyElement {
    indicator(
        if backlog {
            home::Bucket::Backlog
        } else {
            home::bucket(running, attention, None).0
        },
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

/// The label for tasks in no section, in menus and Home's headings alike.
pub(super) const UNGROUPED: &str = "Ungrouped";

/// "Move to section" in a task's right-click menu; ends with "New section…", which moves it there.
pub(super) fn move_submenu(this: &Shark, ws: &str, cx: &Context<Shark>) -> Entry {
    let current = this.section_of(ws);
    let mut entries = vec![];
    for (i, sec) in this.state.sections.iter().enumerate() {
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
            UNGROUPED,
            on_shark(cx, move |s, _, cx| s.move_to_section(&ws, None, cx)),
        ));
    }
    if !entries.is_empty() {
        entries.push(Entry::Sep);
    }
    let ws = ws.to_string();
    entries.push(menu::item(
        IconName::FolderPlus,
        "New section…",
        on_shark(cx, move |s, w, cx| {
            s.start_naming(None, Some(ws.clone()), w, cx)
        }),
    ));
    menu::sub(IconName::FolderInput, "Move to section", entries)
}

/// Right-click menu of a section header.
pub(super) fn section_menu(this: &Shark, i: usize, cx: &Context<Shark>) -> Vec<Entry> {
    let collapsed = this.state.sections.get(i).is_some_and(|s| s.collapsed);
    vec![
        menu::item(
            if collapsed {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            },
            if collapsed { "Expand" } else { "Collapse" },
            on_shark(cx, move |s, _, cx| s.toggle_section(i, cx)),
        ),
        menu::item(
            IconName::Pencil,
            "Rename…",
            on_shark(cx, move |s, w, cx| s.start_naming(Some(i), None, w, cx)),
        ),
        menu::item(
            IconName::Shapes,
            "Change icon…",
            on_shark(cx, move |s, w, cx| s.pick_icon(i, w, cx)),
        ),
        menu::item(
            IconName::FolderPlus,
            "New section…",
            on_shark(cx, |s, w, cx| s.start_naming(None, None, w, cx)),
        ),
        Entry::Sep,
        menu::item(
            IconName::Trash,
            "Delete section",
            on_shark(cx, move |s, w, cx| s.delete_section(i, w, cx)),
        ),
    ]
}

/// Cursor state taken by [`Shark::cursors`]: sidebar row, its task's or section's name,
/// home row.
pub(super) struct Cursors {
    side: Option<usize>,
    ws: Option<String>,
    section: Option<String>,
    home: Option<usize>,
}

/// A sidebar row the cursor can sit on.
#[derive(PartialEq, Clone, Copy, Debug)]
pub(super) enum SideRow {
    /// Index into `state.sections`.
    Section(usize),
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
    pub(super) fn sidebar_task_matches(&self, task: &Task) -> bool {
        self.sidebar_repo_filter
            .as_ref()
            .is_none_or(|filter| home::repo_matches(task, filter))
    }

    fn sidebar_repo_menu(&self, cx: &Context<Self>) -> Vec<Entry> {
        let names: std::collections::BTreeSet<_> = self
            .state
            .tasks
            .iter()
            .flat_map(|t| {
                t.repos
                    .iter()
                    .map(|r| r.name.clone())
                    .chain(t.attaching.iter().map(|r| r.repo.clone()))
            })
            .collect();
        let filters = std::iter::once(None)
            .chain(
                names
                    .into_iter()
                    .map(|name| Some(home::RepoFilter::Repo(name))),
            )
            .chain(
                self.state
                    .tasks
                    .iter()
                    .any(|t| home::repo_matches(t, &home::RepoFilter::NoRepo))
                    .then_some(Some(home::RepoFilter::NoRepo)),
            );
        filters
            .map(|filter| {
                let label = filter
                    .as_ref()
                    .map_or("All repositories", home::RepoFilter::label)
                    .to_string();
                let checked = filter == self.sidebar_repo_filter;
                menu::item(
                    IconName::FolderGit2,
                    label,
                    on_shark(cx, move |s, _, cx| {
                        s.sidebar_repo_filter = filter.clone();
                        s.fix_sel();
                        cx.notify();
                    }),
                )
                .checked(checked)
            })
            .collect()
    }

    pub(super) fn section_of(&self, ws: &str) -> Option<usize> {
        self.state
            .sections
            .iter()
            .position(|s| s.tasks.iter().any(|w| w == ws))
    }

    /// Sidebar rows top to bottom: each section, then its tasks unless it is folded (and not
    /// `unfold`); then the tasks in no section. Without `unfold`, Backlog (except the open
    /// task) and the repository filter hide task rows; section headers always stay.
    pub(super) fn side_rows(&self, unfold: bool) -> Vec<SideRow> {
        let tasks = &self.state.tasks;
        let mut rows = vec![];
        for (i, s) in self.state.sections.iter().enumerate() {
            rows.push(SideRow::Section(i));
            if unfold || !s.collapsed {
                rows.extend(
                    s.tasks
                        .iter()
                        .filter_map(|n| tasks.iter().position(|t| &t.name == n))
                        .map(SideRow::Task),
                );
            }
        }
        rows.extend(
            (0..tasks.len())
                .filter(|&ix| self.section_of(&tasks[ix].name).is_none())
                .map(SideRow::Task),
        );
        rows.retain(|row| match row {
            SideRow::Task(ix) => {
                let task = &tasks[*ix];
                // Global task search uses unfolded rows and keeps every repository reachable.
                unfold
                    || (self.sidebar_task_matches(task)
                        && (!task.backlog || self.active.as_deref() == Some(&task.name)))
            }
            SideRow::Section(_) => true,
        });
        rows
    }

    /// The section under the sidebar cursor, or the cursor task's section.
    pub(super) fn cursor_section(&self) -> Option<usize> {
        match self.sel {
            SideRow::Section(i) => (i < self.state.sections.len()).then_some(i),
            SideRow::Task(ix) => self.section_of(&self.state.tasks.get(ix)?.name),
        }
    }

    pub(super) fn set_collapsed(&mut self, i: usize, collapsed: bool, cx: &mut Context<Self>) {
        if let Some(s) = self.state.sections.get_mut(i)
            && s.collapsed != collapsed
        {
            s.collapsed = collapsed;
            let _ = self.state.save();
            self.fix_sel();
            cx.notify();
        }
    }

    pub(super) fn toggle_section(&mut self, i: usize, cx: &mut Context<Self>) {
        if let Some(s) = self.state.sections.get(i) {
            self.set_collapsed(i, !s.collapsed, cx);
        }
    }

    /// Sidebar and home cursors before a task is removed; see [`Self::restore_cursors`].
    pub(super) fn cursors(&self, cx: &App) -> Cursors {
        Cursors {
            side: self.side_rows(false).iter().position(|r| *r == self.sel),
            // The sidebar cursor follows its task or section, not its row number.
            ws: match self.sel {
                SideRow::Task(ix) => self.state.tasks.get(ix).map(|w| w.name.clone()),
                SideRow::Section(_) => None,
            },
            section: match self.sel {
                SideRow::Section(i) => self.state.sections.get(i).map(|s| s.name.clone()),
                SideRow::Task(_) => None,
            },
            home: self.home_at(&self.home_rows(cx)),
        }
    }

    /// After `name` is removed: a cursor on its row stays at the same height, on whatever moved up into it.
    pub(super) fn restore_cursors(&mut self, name: &str, before: Cursors, cx: &App) {
        let rows = self.side_rows(false);
        let section = (before.section.as_ref())
            .and_then(|c| self.state.sections.iter().position(|s| &s.name == c));
        let moved = (before.ws.as_deref().filter(|c| *c != name))
            .and_then(|c| self.state.tasks.iter().position(|w| w.name == c));
        self.sel = match (section, moved) {
            (Some(i), _) => SideRow::Section(i),
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
                SideRow::Task(ix) if unfold || !self.state.tasks[ix].backlog => Some(ix),
                SideRow::Task(_) | SideRow::Section(_) => None,
            })
            .collect()
    }

    /// Keeps the cursor on a visible row; a task hidden by folding or filtering hands it to
    /// its section's header.
    pub(super) fn fix_sel(&mut self) {
        let rows = self.side_rows(false);
        if rows.contains(&self.sel) {
            return;
        }
        self.sel = (self.cursor_section())
            .map(SideRow::Section)
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
        // Child 0 of the scrolled list is the Tasks header.
        self.sidebar_scroll.scroll_to_item(to + 1);
        cx.notify();
    }

    /// Section actions hit the header under the sidebar cursor, else the section of the task
    /// that rename and archive actions would hit.
    fn target_section(&self, window: &Window) -> Option<usize> {
        match self.sel {
            SideRow::Section(i)
                if self.sidebar_focus.is_focused(window) && i < self.state.sections.len() =>
            {
                Some(i)
            }
            _ => self.section_of(&self.target(window)?.name),
        }
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
                "Put the sidebar cursor on a section or one of its tasks first",
            ),
        }
    }

    /// Left folds the cursor's section and parks the cursor on its header.
    fn collapse_at_cursor(&mut self, cx: &mut Context<Self>) {
        if let Some(i) = self.cursor_section() {
            self.sel = SideRow::Section(i);
            self.set_collapsed(i, true, cx);
            cx.notify();
        }
    }

    pub(super) fn move_to_section(&mut self, ws: &str, to: Option<usize>, cx: &mut Context<Self>) {
        if to.is_some_and(|i| i >= self.state.sections.len()) || self.section_of(ws) == to {
            return;
        }
        self.state.move_to_section(ws, to);
        let _ = self.state.save();
        self.record_interaction(ws, cx);
        self.fix_sel();
        cx.notify();
    }

    /// Dropping a task on another takes its slot, in its section or among the ungrouped.
    pub(super) fn drop_task(&mut self, ws: &str, target: &str, cx: &mut Context<Self>) {
        // The task under the cursor, so the reorder that shifts indices can put it back.
        let cursor = match self.sel {
            SideRow::Task(ix) => self.state.tasks.get(ix).map(|w| w.name.clone()),
            SideRow::Section(_) => None,
        };
        self.state.drop_task(ws, target);
        if let Some(ix) = cursor.and_then(|n| self.state.tasks.iter().position(|w| w.name == n)) {
            self.sel = SideRow::Task(ix);
        }
        let _ = self.state.save();
        self.fix_sel();
        cx.notify();
    }

    pub(super) fn drop_section(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let n = self.state.sections.len();
        if from == to || from >= n || to >= n {
            return;
        }
        let cursor = match self.sel {
            SideRow::Section(j) => self.state.sections.get(j).map(|s| s.name.clone()),
            SideRow::Task(_) => None,
        };
        move_item(&mut self.state.sections, from, to);
        if let Some(j) = cursor.and_then(|c| self.state.sections.iter().position(|s| s.name == c)) {
            self.sel = SideRow::Section(j);
        }
        let _ = self.state.save();
        cx.notify();
    }

    pub(super) fn delete_section(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i >= self.state.sections.len() {
            return;
        }
        let s = self.state.sections.remove(i);
        let _ = self.state.save();
        if let SideRow::Section(j) = self.sel {
            self.sel = SideRow::Section(if j > i { j - 1 } else { j });
        }
        self.fix_sel();
        window.push_notification(format!("Deleted section {} (its tasks stay)", s.name), cx);
        cx.notify();
    }

    /// Names a new section (`rename` is `None`) or renames one, in a dialog.
    pub(super) fn start_naming(
        &mut self,
        rename: Option<usize>,
        then_move: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if rename.is_some_and(|i| i >= self.state.sections.len()) {
            return;
        }
        let original = rename
            .and_then(|i| self.state.sections.get(i))
            .map(|s| s.name.clone());
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Section name")
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
                    "Rename section"
                } else {
                    "New section"
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
                            // Resolve the original name again if sections changed while the dialog was open.
                            let index = original.as_ref().and_then(|name| {
                                s.state.sections.iter().position(|g| &g.name == name)
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
                                    format!("There is already a section {name}"),
                                );
                                return false;
                            }
                            let index = if let Some(i) = index {
                                s.state.sections[i].name = name;
                                i
                            } else {
                                s.state.sections.push(Section {
                                    name,
                                    icon: "folder".into(),
                                    collapsed: false,
                                    tasks: vec![],
                                });
                                let i = s.state.sections.len() - 1;
                                s.sel = SideRow::Section(i);
                                i
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
        // By name: sections may move while the picker is open.
        let name = section.name.clone();
        self.pick_named_icon(
            section.icon.clone(),
            move |s, icon, cx| {
                if let Some(section) = s.state.sections.iter_mut().find(|g| g.name == name) {
                    section.icon = icon;
                    let _ = s.state.save();
                    cx.notify();
                }
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

    /// Sections, then Ungrouped when it is in one, then "New section…".
    pub(super) fn pick_section(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.with_target(window, cx, |s, ws, window, cx| {
            let ws = ws.name;
            let current = s.section_of(&ws);
            let n = s.state.sections.len();
            let mut items: Vec<CommandItem> = (s.state.sections)
                .iter()
                .enumerate()
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
                        .label(UNGROUPED)
                        .icon(IconName::FolderOutput),
                );
            }
            items.push(
                CommandItem::new()
                    .label("New section…")
                    .icon(IconName::FolderPlus),
            );
            let last = items.len() - 1;
            open_menu(
                cx.entity().downgrade(),
                "Move to section…",
                items,
                current.unwrap_or(0),
                move |s, row, window, cx| match row {
                    _ if row == last => s.start_naming(None, Some(ws.clone()), window, cx),
                    _ if row < n => s.move_to_section(&ws, Some(row), cx),
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
            // sessions do. Section tasks indent one step, so their status icon lines up with
            // the section's name.
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
                _ => task_indicator(w.backlog, running, &attn, foreground, ISZ, theme),
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

    /// A section's header: icon, name and fold chevron. Folded, it shows one fact on the right:
    /// how many of its tasks need you, else how many it holds.
    fn section_row(
        &self,
        i: usize,
        sec: &Section,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (theme, rail) = (cx.theme(), self.rail);
        let focused = self.sidebar_focus.is_focused(window);
        let selected = self.sel == SideRow::Section(i);
        let collapsed = sec.collapsed;
        // Counts follow the rows a fold hides: no Backlog, only the filtered repository.
        let tasks: Vec<_> = (sec.tasks.iter())
            .filter_map(|n| self.task(n))
            .filter(|t| !t.backlog && self.sidebar_task_matches(t))
            .collect();
        let needs = (tasks.iter())
            .filter(|t| matches!(self.attention_of(&t.name, cx), Attn::Needs(_)))
            .count();
        let warning = collapsed && needs > 0;
        let tip: SharedString = sec.name.clone().into();
        let badge = match (collapsed, needs) {
            (false, _) => None,
            (true, 1..) => Some(
                h_flex()
                    .id(("section-needs", i))
                    .test_support()
                    .gap_1()
                    .text_xs()
                    .text_color(theme.warning_foreground)
                    .child(Icon::new(IconName::CircleAlert).with_size(px(12.)))
                    .child(needs.to_string())
                    .into_any_element(),
            ),
            (true, 0) if !tasks.is_empty() => Some(
                div()
                    .text_xs()
                    .child(tasks.len().to_string())
                    .into_any_element(),
            ),
            _ => None,
        };
        sidebar_row(rail, theme)
            .relative()
            .id(("section", i))
            .role(gpui_kit::accesskit::Role::Button)
            .aria_label(format!(
                "{} section, {} tasks{}",
                sec.name,
                tasks.len(),
                match needs {
                    0 => String::new(),
                    n => format!(", {n} need you"),
                }
            ))
            .aria_expanded(!collapsed)
            .test_support()
            .map(|d| cursor_row(d, false, selected, focused, theme))
            .when(selected && focused, |d| d.text_color(theme.foreground))
            .map(|d| self.reorder_source(d, reorder::Item::Section(i), sec.name.clone().into()))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.sel = SideRow::Section(i);
                this.toggle_section(i, cx)
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &MouseDownEvent, w, cx| {
                    this.sel = SideRow::Section(i);
                    let entries = section_menu(this, i, cx);
                    this.show_menu(e.position, entries, w, cx);
                }),
            )
            .child(Icon::new(section_icon(&sec.icon)).with_size(ISZ))
            .when(rail && warning, |d| {
                d.child(
                    div()
                        .id(("section-rail-warning", i))
                        .test_support()
                        .absolute()
                        .right(px(-8.))
                        .top(px(10.))
                        .size(px(12.))
                        .text_color(theme.warning_foreground)
                        .child(Icon::new(IconName::CircleAlert).with_size(px(12.))),
                )
            })
            .when(!rail, |d| {
                d.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .child(sec.name.clone()),
                )
                .child(
                    Icon::new(if collapsed {
                        IconName::ChevronRight
                    } else {
                        IconName::ChevronDown
                    })
                    .with_size(px(12.)),
                )
                .child(div().flex_1())
                .children(badge)
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
        for row in self.side_rows(false) {
            match row {
                SideRow::Section(i) => items.push(self.reorder_item(
                    self.section_row(i, &self.state.sections[i], window, cx),
                    reorder::Item::Section(i),
                )),
                SideRow::Task(ix) => {
                    let task = &self.state.tasks[ix];
                    let section = self.section_of(&task.name);
                    items.push(self.reorder_item(
                        self.ws_row(ix, task, section, &order, window, cx),
                        reorder::Item::Task(task.name.clone()),
                    ));
                }
            }
        }
        let new_section_menu = |add_repo: bool| {
            cx.listener(move |this, e: &MouseDownEvent, w, cx| {
                let mut entries = vec![
                    menu::item(
                        IconName::Plus,
                        "New task",
                        on_shark(cx, |s, w, cx| s.new_empty_task(w, cx)),
                    ),
                    menu::item(
                        IconName::FolderPlus,
                        "New section…",
                        on_shark(cx, |s, w, cx| s.start_naming(None, None, w, cx)),
                    ),
                ];
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
        // Dropping a task here takes it out of its section.
        let ws_header = h_flex()
            .id("ws-header")
            .test_support()
            .group("ws-header")
            .justify_between()
            .items_end()
            .child(header("Tasks"))
            .when(!rail, |d| {
                d.child(
                    div()
                        .mr_3()
                        .invisible()
                        .group_hover("ws-header", |s| s.visible())
                        .child(
                            Button::new("new-section")
                                .accessibility_label("New section")
                                .ghost()
                                .xsmall()
                                .text_color(theme.sidebar_foreground)
                                .icon(IconName::FolderPlus)
                                .tooltip("New section")
                                .on_click(cx.listener(|this, _, w, cx| {
                                    this.start_naming(None, None, w, cx)
                                })),
                        ),
                )
            })
            .on_mouse_down(MouseButton::Right, new_section_menu(false));
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
            .on_action(cx.listener(|this, _: &Confirm, window, cx| {
                if !this.side_rows(false).contains(&this.sel) {
                    return;
                }
                match this.sel {
                    SideRow::Task(ix) => this.open_task(ix, window, cx),
                    SideRow::Section(i) => this.toggle_section(i, cx),
                }
            }))
            .on_action(cx.listener(|this, _: &CollapseDir, _, cx| this.collapse_at_cursor(cx)))
            .on_action(cx.listener(|this, _: &ExpandDir, _, cx| {
                if let Some(i) = this.cursor_section() {
                    this.set_collapsed(i, false, cx)
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
                                .child(
                                    side_toggle(
                                        "sidebar-repo-filter",
                                        IconName::ListFilter,
                                        "Filter sidebar by repository",
                                    )
                                    .selected(self.sidebar_repo_filter.is_some())
                                    .on_click(cx.listener(|this, e: &ClickEvent, w, cx| {
                                        let entries = this.sidebar_repo_menu(cx);
                                        this.show_menu(e.position(), entries, w, cx);
                                    }))
                                    .map(|b| quiet_toolbar("sidebar-repo-filter", b)),
                                )
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
                    .child(
                        self.reorder_item(ws_header.into_any_element(), reorder::Item::Ungrouped),
                    )
                    .children(items)
                    .when(
                        self.state.tasks.is_empty() && self.state.sections.is_empty(),
                        |d| d.child(hint("cmd-n to create")),
                    )
                    .when(self.state.repos.is_empty(), |d| d.child(add_repo_row))
                    // The empty space below takes a right-click too.
                    .child(
                        div()
                            .id("sidebar-rest")
                            .flex_1()
                            .min_h(px(24.))
                            .on_mouse_down(MouseButton::Right, new_section_menu(true)),
                    ),
            )
            .child(self.render_usage_footer(cx))
            .test_support()
    }
}
