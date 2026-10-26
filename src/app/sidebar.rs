//! The sidebar: sections, workspace and repo rows, the cursor, drag and drop.
use super::*;

/// Icons a section can have, by Lucide name (what `Section::icon` stores).
const SECTION_ICONS: &[(&str, IconName)] = &[
    ("folder", IconName::Folder),
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

/// "Move to section" in a workspace's right-click menu; ends with "New section", which moves it there.
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
            "No section",
            on_shark(cx, move |s, _, cx| s.move_to_section(&ws, None, cx)),
        ));
    }
    if !entries.is_empty() {
        entries.push(Entry::Sep);
    }
    let ws = ws.to_string();
    entries.push(menu::item(
        IconName::FolderPlus,
        "New section",
        on_shark(cx, move |s, w, cx| {
            s.start_naming(None, Some(ws.clone()), w, cx)
        }),
    ));
    menu::sub(IconName::FolderInput, "Move to section", entries)
}

/// Right-click menu of a section header.
fn section_menu(this: &Shark, i: usize, cx: &Context<Shark>) -> Vec<Entry> {
    let collapsed = this.state.sections.get(i).is_some_and(|s| s.collapsed);
    vec![
        menu::item(
            if collapsed { IconName::ChevronDown } else { IconName::ChevronRight },
            if collapsed { "Expand" } else { "Collapse" },
            on_shark(cx, move |s, _, cx| s.toggle_section(i, cx)),
        ),
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
            "New section",
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

/// A sidebar row the cursor can sit on.
#[derive(PartialEq, Clone, Copy, Debug)]
pub(super) enum SideRow {
    /// Index into `state.sections`.
    Section(usize),
    /// Index into `state.workspaces`.
    Ws(usize),
}

/// The inline section name field in the sidebar.
pub(super) struct Naming {
    /// The section being renamed; `None` adds one.
    rename: Option<usize>,
    /// Workspace to move into the new section ("Move to Section › New Section…").
    then_move: Option<String>,
    input: Entity<InputState>,
    _sub: Subscription,
}

/// A workspace row being dragged: onto a header to move it in, onto a row to take its slot.
#[derive(Clone)]
struct DraggedWs(SharedString);

/// A section header being dragged onto another to take its place.
#[derive(Clone)]
struct DraggedSection(usize, SharedString);

impl Render for DraggedWs {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        drag_chip(self.0.clone(), cx)
    }
}

impl Render for DraggedSection {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        drag_chip(self.1.clone(), cx)
    }
}

fn drag_chip(label: SharedString, cx: &App) -> Div {
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
            .position(|s| s.workspaces.iter().any(|w| w == ws))
    }

    /// Sidebar rows top to bottom: each section, then its workspaces unless it is
    /// collapsed (and not `unfold`); then the workspaces in no section.
    pub(super) fn side_rows(&self, unfold: bool) -> Vec<SideRow> {
        let ws = &self.state.workspaces;
        let mut rows = vec![];
        for (i, s) in self.state.sections.iter().enumerate() {
            rows.push(SideRow::Section(i));
            if unfold || !s.collapsed {
                rows.extend(
                    s.workspaces
                        .iter()
                        .filter_map(|n| ws.iter().position(|w| &w.name == n))
                        .map(SideRow::Ws),
                );
            }
        }
        rows.extend(
            (0..ws.len())
                .filter(|&ix| self.section_of(&ws[ix].name).is_none())
                .map(SideRow::Ws),
        );
        rows
    }

    /// Every workspace in sidebar order, folded sections included (`cmd-j` walks this).
    pub(crate) fn sidebar_order(&self) -> Vec<usize> {
        self.ws_order(true)
    }

    /// Workspaces in sidebar order, folded ones only with `unfold`; cmd-N opens the Nth visible.
    pub(super) fn ws_order(&self, unfold: bool) -> Vec<usize> {
        self.side_rows(unfold)
            .into_iter()
            .filter_map(|r| match r {
                SideRow::Ws(ix) => Some(ix),
                SideRow::Section(_) => None,
            })
            .collect()
    }

    /// Keeps the cursor on a visible row; a workspace hidden by collapsing hands it to its header.
    fn fix_sel(&mut self) {
        let rows = self.side_rows(false);
        if rows.contains(&self.sel) {
            return;
        }
        self.sel = (self.cursor_section())
            .map(SideRow::Section)
            .or(rows.first().copied())
            .unwrap_or(SideRow::Ws(0));
    }

    fn move_sel(&mut self, to: impl FnOnce(usize, usize) -> usize, cx: &mut Context<Self>) {
        let rows = self.side_rows(false);
        if rows.is_empty() {
            return;
        }
        let at = rows.iter().position(|r| *r == self.sel).unwrap_or(0);
        let to = to(at, rows.len()).min(rows.len() - 1);
        self.sel = rows[to];
        // Child 0 of the scrolled list is the Workspaces header.
        self.sidebar_scroll.scroll_to_item(to + 1);
        cx.notify();
    }

    /// The section under the sidebar cursor, or the cursor workspace's section.
    fn cursor_section(&self) -> Option<usize> {
        match self.sel {
            SideRow::Section(i) => (i < self.state.sections.len()).then_some(i),
            SideRow::Ws(ix) => self.section_of(&self.state.workspaces.get(ix)?.name),
        }
    }

    /// Section actions hit the cursor's section when the sidebar is focused, else the open
    /// workspace's.
    fn target_section(&self, window: &Window) -> Option<usize> {
        if self.sidebar_focus.is_focused(window) {
            return self.cursor_section();
        }
        self.section_of(self.active.as_ref()?)
    }

    pub(super) fn with_section(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Self, usize, &mut Window, &mut Context<Self>),
    ) {
        match self.target_section(window) {
            Some(i) => f(self, i, window, cx),
            None => notify_err(window, cx, "Put the sidebar cursor on a section first"),
        }
    }

    fn set_collapsed(&mut self, i: usize, collapsed: bool, cx: &mut Context<Self>) {
        if let Some(s) = self.state.sections.get_mut(i)
            && s.collapsed != collapsed
        {
            s.collapsed = collapsed;
            self.state.save();
            self.fix_sel();
            cx.notify();
        }
    }

    pub(super) fn toggle_section(&mut self, i: usize, cx: &mut Context<Self>) {
        if let Some(s) = self.state.sections.get(i) {
            self.set_collapsed(i, !s.collapsed, cx);
        }
    }

    /// `h` in the sidebar: fold the cursor's section and park the cursor on its header.
    fn collapse_at_cursor(&mut self, cx: &mut Context<Self>) {
        if let Some(i) = self.cursor_section() {
            self.sel = SideRow::Section(i);
            self.set_collapsed(i, true, cx);
            cx.notify();
        }
    }

    fn move_to_section(&mut self, ws: &str, to: Option<usize>, cx: &mut Context<Self>) {
        if self.section_of(ws) == to {
            return;
        }
        for s in &mut self.state.sections {
            s.workspaces.retain(|w| w != ws);
        }
        if let Some(s) = to.and_then(|i| self.state.sections.get_mut(i)) {
            s.workspaces.push(ws.to_string());
        }
        self.state.save();
        self.fix_sel();
        cx.notify();
    }

    fn drop_ws(&mut self, ws: &str, target: &str, cx: &mut Context<Self>) {
        // The workspace under the cursor, so the reorder that shifts indices can put it back.
        let cursor = match self.sel {
            SideRow::Ws(ix) => self.state.workspaces.get(ix).map(|w| w.name.clone()),
            SideRow::Section(_) => None,
        };
        self.state.drop_ws(ws, target);
        if let Some(ix) =
            cursor.and_then(|n| self.state.workspaces.iter().position(|w| w.name == n))
        {
            self.sel = SideRow::Ws(ix);
        }
        self.state.save();
        self.fix_sel();
        cx.notify();
    }

    fn drop_section(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let n = self.state.sections.len();
        if from == to || from >= n || to >= n {
            return;
        }
        let cursor = match self.sel {
            SideRow::Section(j) => Some(self.state.sections[j].name.clone()),
            SideRow::Ws(_) => None,
        };
        move_item(&mut self.state.sections, from, to);
        if let Some(j) = cursor.and_then(|c| self.state.sections.iter().position(|s| s.name == c)) {
            self.sel = SideRow::Section(j);
        }
        if self.naming.as_ref().is_some_and(|n| n.rename.is_some()) {
            self.naming = None; // its index moved
        }
        self.state.save();
        cx.notify();
    }

    pub(super) fn delete_section(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i >= self.state.sections.len() {
            return;
        }
        let s = self.state.sections.remove(i);
        self.state.save();
        self.naming = None;
        if let SideRow::Section(j) = self.sel {
            self.sel = SideRow::Section(if j > i { j - 1 } else { j });
        }
        self.fix_sel();
        window.push_notification(
            format!("Deleted section {} (its workspaces stay)", s.name),
            cx,
        );
        cx.notify();
    }

    /// Opens the inline name field: on section `rename`, or below the sections for a new one.
    pub(super) fn start_naming(
        &mut self,
        rename: Option<usize>,
        then_move: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value = rename
            .and_then(|i| self.state.sections.get(i))
            .map(|s| s.name.clone())
            .unwrap_or_default();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Section name")
                .default_value(value)
        });
        let _sub = cx.subscribe_in(
            &input,
            window,
            |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter { .. } => this.commit_naming(true, window, cx),
                InputEvent::Blur => this.commit_naming(false, window, cx),
                _ => {}
            },
        );
        // Deferred: a closing menu or palette restores focus first, which would blur the field.
        let field = input.clone();
        window.defer(cx, move |window, cx| {
            field.update(cx, |i, cx| {
                i.focus(window, cx);
                i.select_all(window, cx);
            })
        });
        self.rail = false;
        self.naming = Some(Naming {
            rename,
            then_move,
            input,
            _sub,
        });
        cx.notify();
    }

    /// Enter saves (a taken name stays in the field); clicking away saves too.
    /// An empty name changes nothing.
    fn commit_naming(&mut self, enter: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(n) = &self.naming else {
            return;
        };
        let name = n.input.read(cx).value().trim().to_string();
        let taken = self
            .state
            .sections
            .iter()
            .enumerate()
            .any(|(i, s)| s.name == name && Some(i) != n.rename);
        if taken && enter {
            return notify_err(window, cx, format!("There is already a section {name}"));
        }
        let Some(n) = self.naming.take() else {
            return;
        };
        if !name.is_empty() && !taken {
            match n.rename {
                Some(i) => {
                    if let Some(s) = self.state.sections.get_mut(i) {
                        s.name = name;
                    }
                }
                None => {
                    self.state.sections.push(Section {
                        name,
                        icon: "folder".into(),
                        collapsed: false,
                        workspaces: vec![],
                    });
                    let i = self.state.sections.len() - 1;
                    self.sel = SideRow::Section(i);
                    if let Some(ws) = n.then_move {
                        self.move_to_section(&ws, Some(i), cx);
                    }
                }
            }
            self.state.save();
        }
        if enter {
            self.sidebar_focus.focus(window, cx);
        }
        cx.notify();
    }

    fn cancel_naming(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.naming = None;
        self.sidebar_focus.focus(window, cx);
        cx.notify();
    }

    pub(super) fn pick_icon(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(current) = self.state.sections.get(i).map(|s| s.icon.clone()) else {
            return;
        };
        // Named by keyword, so typing "bug" or "star" finds the icon.
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
            move |s, row, _, cx| {
                if let Some(sec) = s.state.sections.get_mut(i) {
                    sec.icon = SECTION_ICONS[row].0.into();
                    s.state.save();
                    cx.notify();
                }
            },
            window,
            cx,
        );
    }

    /// Sections, then "No section" when it is in one, then "New section".
    pub(super) fn pick_section(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.target(window).map(|w| w.name) else {
            return notify_err(window, cx, "Open a workspace first");
        };
        let current = self.section_of(&ws);
        let n = self.state.sections.len();
        let mut items: Vec<CommandItem> = self
            .state
            .sections
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
                    .label("No section")
                    .icon(IconName::FolderOutput),
            );
        }
        items.push(
            CommandItem::new()
                .label("New section")
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
    }

    pub(super) fn render_sidebar(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let focused = self.sidebar_focus.is_focused(window);
        let rail = self.rail;
        // The rail: 32px icon buttons, 16px icons, 16 apart.
        let isz = px(if rail { 16. } else { 14. });
        let header = |t: &'static str| {
            div()
                .when(rail, |d| d.hidden())
                .px_4()
                .pt_4()
                .pb_1()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.muted_foreground)
                .child(t)
        };
        let hint = |t: &'static str| {
            div()
                .when(rail, |d| d.hidden())
                .px_4()
                .text_xs()
                .text_color(faint(theme))
                .child(t)
        };
        let row = || {
            h_flex()
                .h(px(28.))
                .flex_none()
                .mx_2()
                .mb_1() // so a hovered row never touches the selected one
                .px_2()
                .gap_2()
                .rounded(theme.radius)
                .text_sm()
                .when(rail, rail_button)
        };
        let m = window.modifiers();
        let cmd_held = m.platform && m.number_of_modifiers() == 1;
        let repos = self.state.repos.iter().enumerate().map(|(ix, r)| {
            let (name, path) = (r.name.clone(), r.path.clone());
            let tip: SharedString = name.clone().into();
            row()
                .id(("repo", ix))
                .text_color(theme.muted_foreground)
                .hover(|d| d.bg(theme.sidebar_accent))
                .child(Icon::new(IconName::FolderGit2).with_size(isz))
                .when(!rail, |d| d.child(div().truncate().child(r.name.clone())))
                .when(rail, |d| {
                    d.tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
                })
                .on_click({
                    let name = name.clone();
                    cx.listener(move |this, _, w, cx| {
                        this.new_workspace(vec![name.clone()], None, w, cx)
                    })
                })
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, e: &MouseDownEvent, w, cx| {
                        let (p1, p3) = (path.clone(), path.clone());
                        let picked = vec![name.clone()];
                        let mut entries = vec![
                            menu::item(
                                IconName::Plus,
                                "New workspace…",
                                on_shark(cx, move |s, w, cx| {
                                    s.new_workspace(picked.clone(), None, w, cx)
                                }),
                            ),
                            Entry::Sep,
                            menu::item(IconName::FolderOpen, "Reveal in Finder", move |_, cx| {
                                cx.reveal_path(&p1)
                            })
                            .hint(tilde(&path)),
                        ];
                        // Active workspace first: the likeliest target.
                        let mut targets: Vec<&Workspace> = (this.state.workspaces.iter())
                            .filter(|w| !w.repos.iter().any(|r| r.name == name))
                            .collect();
                        targets.sort_by_key(|w| this.active.as_ref() != Some(&w.name));
                        if let Some(repo) = this.state.repos.iter().find(|r| r.name == name) {
                            let rows: Vec<Entry> = (targets.into_iter())
                                .map(|w| {
                                    let (ws, repo) = (w.name.clone(), repo.clone());
                                    menu::item(
                                        IconName::MessageSquare,
                                        w.name.clone(),
                                        on_shark(cx, move |s, win, cx| {
                                            s.add_repo_to_workspace(ws.clone(), repo.clone(), win, cx)
                                        }),
                                    )
                                })
                                .collect();
                            if !rows.is_empty() {
                                entries.insert(
                                    1,
                                    menu::sub(IconName::FolderPlus, "Add to workspace", rows),
                                );
                            }
                        }
                        entries.extend(open_in_entries(None, &[(name.clone(), path.clone())]));
                        entries.push(
                            menu::item(IconName::Copy, "Copy path", move |w, cx| {
                                copy(p3.display().to_string(), w, cx)
                            })
                            .hint(tilde(&path)),
                        );
                        this.show_menu(e.position, entries, w, cx);
                    }),
                )
        });
        let order = self.ws_order(false);
        let (primary, accent) = (theme.primary, theme.sidebar_accent);
        // Dropping a workspace on a header or on a row of a section moves it there.
        let drop_on = |to: Option<usize>| {
            cx.listener(move |this: &mut Shark, d: &DraggedWs, _, cx| {
                this.move_to_section(&d.0, to, cx)
            })
        };
        let ws_row = |ix: usize, w: &Workspace, section: Option<usize>| {
            let running = self.running_chats(&w.name, cx);
            let busy = running > 0;
            let selected = self.sel == SideRow::Ws(ix);
            let active = self.active.as_ref() == Some(&w.name);
            let attn = self.attention_of(&w.name, cx);
            let nth = order.iter().position(|&o| o == ix).filter(|&n| n < 9);
            let ws = w.clone();
            row()
                .id(ix)
                // The rail shows only the icon, so rows keep the one-line height there.
                .when(!rail, |d| d.h(px(44.)))
                .when(section.is_some() && !rail, |d| d.ml(px(24.)))
                .map(|d| cursor_row(d, selected, focused, theme.sidebar_accent, theme))
                // The cursor moves on press, not release: a held or dragged row is the one ringed.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.sel = SideRow::Ws(ix);
                        cx.notify();
                    }),
                )
                .on_click(
                    cx.listener(move |this, _, window, cx| this.open_workspace(ix, window, cx)),
                )
                // Right-click moves the cursor too, so it's clear which row the menu is for.
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, e: &MouseDownEvent, w, cx| {
                        this.sel = SideRow::Ws(ix);
                        let entries = ws_menu(this, &ws, cx);
                        this.show_menu(e.position, entries, w, cx);
                    }),
                )
                .on_drag(DraggedWs(w.name.clone().into()), |d, _, _, cx| {
                    cx.new(|_| d.clone())
                })
                .drag_over::<DraggedWs>(move |d, _, _, _| d.border_color(primary))
                .on_drop({
                    let target = w.name.clone();
                    cx.listener(move |this, d: &DraggedWs, _, cx| this.drop_ws(&d.0, &target, cx))
                })
                .child(match (busy, nth) {
                    (_, Some(n)) if cmd_held => kbd(&format!("cmd-{}", n + 1)).into_any_element(),
                    (true, _) => Spinner::new()
                        .icon(IconName::LoaderCircle)
                        .color(theme.primary)
                        .with_size(isz)
                        .into_any_element(),
                    (false, _) => {
                        let (icon, color) = match &attn {
                            Attn::Needs(_) => (IconName::CircleAlert, theme.warning_foreground),
                            Attn::Unread => (IconName::CircleCheck, theme.success_foreground),
                            Attn::None => (IconName::CircleDashed, theme.muted_foreground),
                        };
                        Icon::new(icon)
                            .with_size(isz)
                            .text_color(color)
                            .into_any_element()
                    }
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
                                    .child(w.name.clone()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child({
                                        let repos = (w.repos.iter())
                                            .map(|r| r.name.as_str())
                                            .collect::<Vec<_>>()
                                            .join(" · ");
                                        match (&attn, running) {
                                            // The step says where, and that it's waiting on you.
                                            (Attn::Needs(step), _) => step.clone(),
                                            (_, 0 | 1) => repos,
                                            (_, n) => format!("{n} chats running · {repos}"),
                                        }
                                    }),
                            ),
                    )
                })
                .when(rail, |d| {
                    let name: SharedString = w.name.clone().into();
                    d.tooltip(move |w, cx| Tooltip::new(name.clone()).build(w, cx))
                })
                // Holding cmd in the rail names every workspace beside its cmd-N key.
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
                                    .child(w.name.clone()),
                            ),
                        )),
                    )
                })
                .into_any_element()
        };
        let section_row =
            |i: usize, sec: &Section| {
                let selected = self.sel == SideRow::Section(i);
                let tip: SharedString = sec.name.clone().into();
                // A folded section still shows that a workspace inside needs you.
                let needs = sec.collapsed
                    && sec
                        .workspaces
                        .iter()
                        .any(|n| matches!(self.attention_of(n, cx), Attn::Needs(_)));
                row()
                    .id(("section", i))
                    .text_color(theme.muted_foreground)
                    .map(|d| cursor_row(d, selected, focused, theme.sidebar_accent, theme))
                    .drag_over::<DraggedWs>(move |d, _, _, _| d.bg(accent).border_color(primary))
                    .on_drop(drop_on(Some(i)))
                    .on_drag(DraggedSection(i, sec.name.clone().into()), |d, _, _, cx| {
                        cx.new(|_| d.clone())
                    })
                    .drag_over::<DraggedSection>(move |d, _, _, _| d.border_color(primary))
                    .on_drop(cx.listener(move |this, d: &DraggedSection, _, cx| {
                        this.drop_section(d.0, i, cx)
                    }))
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
                    .child(
                        Icon::new(section_icon(&sec.icon))
                            .with_size(isz)
                            .when(needs, |i| i.text_color(theme.warning_foreground)),
                    )
                    .when(!rail, |d| {
                        d.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_weight(FontWeight::MEDIUM)
                                .child(sec.name.clone()),
                        )
                        .when(sec.collapsed, |d| {
                            d.child(div().text_xs().child(sec.workspaces.len().to_string()))
                        })
                        .child(
                            Icon::new(match sec.collapsed {
                                true => IconName::ChevronRight,
                                false => IconName::ChevronDown,
                            })
                            .with_size(px(12.)),
                        )
                    })
                    .when(rail, |d| {
                        d.tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
                    })
                    .into_any_element()
            };
        let name_row = |input: &Entity<InputState>, icon: IconName| {
            row()
                .key_context("SectionName")
                .border_1()
                .border_color(theme.primary)
                .bg(theme.list_active)
                .text_color(theme.muted_foreground)
                .on_action(cx.listener(|this, _: &CancelEdit, w, cx| this.cancel_naming(w, cx)))
                .child(Icon::new(icon).with_size(px(14.)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(theme.foreground)
                        .child(Input::new(input).appearance(false).small()),
                )
                .into_any_element()
        };
        let mut items: Vec<AnyElement> = vec![];
        let mut new_row = self
            .naming
            .as_ref()
            .filter(|n| n.rename.is_none())
            .map(|n| name_row(&n.input, IconName::Folder));
        for r in self.side_rows(false) {
            match r {
                SideRow::Section(i) => {
                    let sec = &self.state.sections[i];
                    match &self.naming {
                        Some(n) if n.rename == Some(i) => {
                            items.push(name_row(&n.input, section_icon(&sec.icon)))
                        }
                        _ => items.push(section_row(i, sec)),
                    }
                }
                SideRow::Ws(ix) => {
                    let w = &self.state.workspaces[ix];
                    let section = self.section_of(&w.name);
                    if section.is_none() {
                        // A new section's field goes below the existing sections.
                        items.extend(new_row.take());
                    }
                    items.push(ws_row(ix, w, section));
                }
            }
        }
        items.extend(new_row.take());
        let new_section_menu = |add_repo: bool| {
            cx.listener(move |this, e: &MouseDownEvent, w, cx| {
                let mut entries = vec![
                    menu::item(
                        IconName::Plus,
                        "New workspace…",
                        on_shark(cx, |s, w, cx| {
                            s.new_workspace(s.state.last_repos.clone(), None, w, cx)
                        }),
                    ),
                    menu::item(
                        IconName::FolderPlus,
                        "New section",
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
        let ws_header = h_flex()
            .id("ws-header")
            .group("ws-header")
            .justify_between()
            .items_end()
            .drag_over::<DraggedWs>(move |d, _, _, _| d.bg(accent))
            .on_drop(drop_on(None))
            .child(header("Workspaces"))
            .when(!rail, |d| {
                d.child(
                    div()
                        .mr_3()
                        .invisible()
                        .group_hover("ws-header", |s| s.visible())
                        .child(
                            Button::new("new-section")
                                .ghost()
                                .xsmall()
                                .icon(IconName::FolderPlus)
                                .tooltip("New section")
                                .on_click(cx.listener(|this, _, w, cx| {
                                    this.start_naming(None, None, w, cx)
                                })),
                        ),
                )
            })
            .on_mouse_down(MouseButton::Right, new_section_menu(false));
        // Like the Workspaces header, but the + stays visible (dimmed) so a second repo is
        // findable; right-click offers the same.
        let repo_header = h_flex()
            .id("repo-header")
            .group("repo-header")
            .justify_between()
            .items_end()
            .child(header("Repos"))
            .when(!rail, |d| {
                d.child(
                    div()
                        .mr_3()
                        .opacity(0.5)
                        .group_hover("repo-header", |s| s.opacity(1.))
                        .child(
                            Button::new("add-repo")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Plus)
                                .tooltip("Add repo, or drop folders here")
                                .on_click(cx.listener(|this, _, w, cx| this.add_repo(w, cx))),
                        ),
                )
            })
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, e: &MouseDownEvent, w, cx| {
                    let entries = vec![menu::item(
                        IconName::FolderPlus,
                        "Add repo…",
                        on_shark(cx, |s, w, cx| s.add_repo(w, cx)),
                    )];
                    this.show_menu(e.position, entries, w, cx);
                }),
            );
        let add_repo_row = row()
            .id("add-repo-row")
            .text_color(theme.muted_foreground)
            .hover(|d| d.bg(theme.sidebar_accent))
            .child(Icon::new(IconName::FolderPlus).with_size(isz))
            .when(!rail, |d| {
                d.child(div().flex_1().child("Add repo…"))
            })
            .when(rail, |d| {
                d.tooltip(|w, cx| Tooltip::new("Add repo").build(w, cx))
            })
            .on_click(cx.listener(|this, _, w, cx| this.add_repo(w, cx)));
        v_flex()
            .id("sidebar")
            // Folders dragged in from Finder are added as repos.
            .drag_over::<ExternalPaths>(move |d, _, _, _| d.bg(accent))
            .on_drop(
                cx.listener(|this, p: &ExternalPaths, w, cx| this.add_repo_paths(p.paths(), w, cx)),
            )
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
            .text_color(theme.sidebar_foreground)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_sel(|at, _| at + 1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| {
                this.move_sel(|at, _| at.saturating_sub(1), cx)
            }))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| this.move_sel(|_, _| 0, cx)))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| this.move_sel(|_, n| n - 1, cx)))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| match this.sel {
                SideRow::Ws(ix) => this.open_workspace(ix, window, cx),
                SideRow::Section(i) => this.toggle_section(i, cx),
            }))
            .on_action(cx.listener(|this, _: &CollapseDir, _, cx| this.collapse_at_cursor(cx)))
            .on_action(cx.listener(|this, _: &ExpandDir, _, cx| {
                if let SideRow::Section(i) = this.sel {
                    this.set_collapsed(i, false, cx)
                }
            }))
            // Room for the traffic lights; the transparent titlebar lets the sidebar run under them.
            .child(top_strip())
            .child(
                h_flex()
                    .h(px(28.))
                    .flex_none()
                    .pl(px(20.))
                    .mb(px(if rail { 16. } else { 4. }))
                    .window_control_area(WindowControlArea::Drag)
                    .child(
                        side_toggle(
                            "toggle-sidebar",
                            match rail {
                                true => IconName::PanelLeftOpen,
                                false => IconName::PanelLeftClose,
                            },
                            "Toggle sidebar (cmd-b)",
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
                    ),
            )
            .child(self.render_home_row(rail, cx))
            .child(
                // Children are indexed by `move_sel`: the header, then one per `side_rows` row.
                v_flex()
                    .id("sidebar-rows")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.sidebar_scroll)
                    .child(ws_header)
                    .children(items)
                    .when(
                        self.state.workspaces.is_empty() && self.state.sections.is_empty(),
                        |d| {
                            d.child(hint(match self.state.repos.is_empty() {
                                true => "Add a repo first",
                                false => "cmd-n to create",
                            }))
                        },
                    )
                    .child(repo_header)
                    .children(repos)
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
            .test_support()
    }
}
