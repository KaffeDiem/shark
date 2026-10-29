//! Side-by-side tab groups: which group is focused, what each shows, and moving tabs between
//! them. `Workspace::layout` is the saved truth; `tab`, `editor` and the active chat mirror the
//! focused group's selection.
use super::*;

/// Neither group may get narrower than this.
pub(super) const GROUP_MIN: Pixels = px(160.);

impl Shark {
    /// The focused group's content as a tab identity.
    pub(super) fn shown_id(&self, cx: &App) -> Option<TabId> {
        let ws = self.active_ws()?;
        match self.tab {
            Tab::Chat => ws
                .chats
                .get(ws.active_chat)
                .map(|c| TabId::Chat(c.id.clone())),
            Tab::Feature(f) => Some(f.tab_id()),
            Tab::File => match &self.editor {
                Some(e) => Some(TabId::File(e.read(cx).path.clone())),
                // A file still being restored.
                None => (ws.layout.selected[ws.layout.focused].clone())
                    .filter(|t| matches!(t, TabId::File(_))),
            },
        }
    }

    pub(super) fn focused_group(&self) -> usize {
        self.active_ws().map_or(0, |ws| ws.layout.focused)
    }

    /// Two groups are on screen.
    pub(super) fn split_shown(&self) -> bool {
        !self.home_shown() && self.active_ws().is_some_and(Workspace::split)
    }

    /// The groups shown, left to right.
    pub(super) fn groups(&self) -> std::ops::Range<usize> {
        0..if self.split_shown() { 2 } else { 1 }
    }

    /// Group `g`'s selected tab; the focused group's is what `tab` shows.
    pub(super) fn group_selection(&self, g: usize, cx: &App) -> Option<TabId> {
        let ws = self.active_ws()?;
        match g == ws.layout.focused {
            true => self.shown_id(cx),
            false => ws.layout.selected[g].clone(),
        }
    }

    /// The tab is selected in a group on screen.
    pub(super) fn tab_visible(&self, id: &TabId, cx: &App) -> bool {
        !self.home_shown()
            && self
                .groups()
                .any(|g| self.group_selection(g, cx).as_ref() == Some(id))
    }

    /// The chat is the focused one, or shown in the other group: its turns aren't unread.
    pub(super) fn chat_on_screen(&self, chat: &Entity<Chat>, cx: &App) -> bool {
        let id = TabId::Chat(chat.read(cx).meta.id.clone());
        self.active_chat() == Some(chat) || (self.split_shown() && self.tab_visible(&id, cx))
    }

    /// Records the shown tab as its group's selection and focuses that group.
    pub(super) fn record_selection(&mut self, cx: &App) {
        let Some(id) = self.shown_id(cx) else { return };
        let Some(ws) = self.active_ws_mut() else {
            return;
        };
        let before = ws.layout.clone();
        ws.select_tab(&id);
        if ws.layout != before {
            let _ = self.state.save();
        }
    }

    /// Makes group `g` the focused one without moving keyboard focus: `tab`, `editor` and the
    /// active chat follow its selection. Sidebar, rail and dialogs never call this, so they
    /// keep the last content group.
    pub(super) fn adopt_group(&mut self, g: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.active_ws_mut() else {
            return;
        };
        if !ws.split() || ws.layout.focused == g || g > 1 {
            return;
        }
        ws.layout.focused = g;
        let sel = ws.layout.selected[g].clone();
        let _ = self.state.save();
        self.apply_selection(sel, window, cx);
    }

    /// Points `tab`, `editor` and the active chat at `sel` without moving keyboard focus.
    fn apply_selection(&mut self, sel: Option<TabId>, window: &mut Window, cx: &mut Context<Self>) {
        match sel {
            Some(TabId::Chat(id)) => {
                let Some(ws) = self.active_ws_mut() else {
                    return;
                };
                let Some(ix) = ws.chats.iter().position(|c| c.id == id) else {
                    return;
                };
                ws.active_chat = ix;
                let _ = self.state.save();
                self.tab = Tab::Chat;
                let chat = self.ensure_group_chat(&id, window, cx);
                if let (Some(chat), Some(sync)) = (chat, self.active_sync().cloned()) {
                    sync.update(cx, |g, cx| g.set_chat(chat, cx));
                }
            }
            Some(TabId::Diff) => self.tab = Tab::Feature(Feature::Diff),
            Some(TabId::Git) => {
                self.ensure_git_feature(window, cx);
                self.tab = Tab::Feature(Feature::Git);
            }
            Some(TabId::File(path)) => {
                self.editor = self.editor_for(&path, cx);
                self.tab = Tab::File;
            }
            None => {}
        }
        self.remember_open();
        self.configure_differ(cx);
        cx.notify();
    }

    pub(super) fn editor_for(&self, path: &str, cx: &App) -> Option<Entity<FileEditor>> {
        (self.file_tabs.iter())
            .find(|t| t.editor.read(cx).path == path)
            .map(|t| t.editor.clone())
    }

    /// The chat entity of an open chat of the active workspace, started if need be; marks it seen.
    fn ensure_group_chat(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<Chat>> {
        let ws = self.active_ws()?.clone();
        let meta = ws.chats.iter().find(|c| c.id == id)?.clone();
        self.unread.remove(id);
        Some(self.ensure_chat(&ws, &meta, window, cx))
    }

    /// The unfocused group's selection, made renderable: its chat started, its Git loaded.
    pub(super) fn prepare_other_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.active_ws().filter(|ws| ws.split()) else {
            return;
        };
        match ws.layout.selected[1 - ws.layout.focused].clone() {
            Some(TabId::Chat(id)) => {
                self.ensure_group_chat(&id, window, cx);
            }
            Some(TabId::Git) => {
                self.ensure_git_feature(window, cx);
            }
            _ => {}
        }
    }

    /// Shows `id` in its group, focusing both; a file still restoring shows its loading state.
    pub(super) fn select_id(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.active_ws() else { return };
        match &id {
            TabId::Chat(c) => {
                if let Some(ix) = ws.chats.iter().position(|m| m.id == *c) {
                    self.select_chat(ix, window, cx)
                }
            }
            TabId::Diff => self.show(Tab::Feature(Feature::Diff), window, cx),
            TabId::Git => self.show(Tab::Feature(Feature::Git), window, cx),
            TabId::File(path) => {
                match (self.file_tabs.iter()).position(|t| t.editor.read(cx).path == *path) {
                    Some(ix) => {
                        self.select_file(ix, window, cx);
                        // Refused (edited inline in Diff): the layout follows what is shown.
                        if self.shown_id(cx).as_ref() != Some(&id) {
                            self.record_selection(cx);
                        }
                    }
                    None if ws.tab_order.contains(&id) => {
                        let ws = self.active_ws_mut().unwrap();
                        ws.select_tab(&id);
                        let g = ws.layout.focused;
                        let _ = self.state.save();
                        self.editor = None;
                        self.tab = Tab::File;
                        self.home.on = false;
                        self.remember_open();
                        self.group_focus[g].focus(window, cx);
                        cx.notify();
                    }
                    None => {}
                }
            }
        }
    }

    /// After a close or move changed the layout: the focused group shows its selection, and
    /// keyboard focus follows when `refocus` (the focused group's shown tab went away).
    pub(super) fn sync_groups(
        &mut self,
        refocus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.active_ws() else { return };
        let sel = ws.layout.selected[ws.layout.focused].clone();
        match (refocus, sel) {
            (true, Some(id)) => self.select_id(id, window, cx),
            (false, sel) if sel != self.shown_id(cx) => self.apply_selection(sel, window, cx),
            _ => {}
        }
        self.prepare_other_group(window, cx);
        self.reveal_tab(cx);
        cx.notify();
    }

    /// Whether `tab` can go to group `to`: a new right group needs another visible tab left.
    pub(super) fn can_move_to(&self, tab: &TabId, to: usize, cx: &App) -> bool {
        let Some(ws) = self.active_ws() else {
            return false;
        };
        if !ws.split() {
            return match to {
                // Within the one group: to its end.
                0 => ws.tab_order.contains(tab),
                _ => to == 1 && self.visible_tab_ids(ws, 0, cx).iter().any(|t| t != tab),
            };
        }
        // The group left behind closes, or keeps something to show.
        let from = ws.group_of(tab);
        let rest = ws.group_tabs(from).into_iter().filter(|t| t != tab).count();
        ws.tab_order.contains(tab)
            && to < 2
            && (from == to
                || rest == 0
                || self.visible_tab_ids(ws, from, cx).iter().any(|t| t != tab))
    }

    /// Moves `tab` of workspace `workspace` to group `to` (before `before`, else at its end),
    /// then shows and focuses it. Stale identities change nothing.
    pub(super) fn move_to_group(
        &mut self,
        workspace: &str,
        tab: &TabId,
        to: usize,
        before: Option<&TabId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active.as_deref() != Some(workspace) || !self.can_move_to(tab, to, cx) {
            return;
        }
        let ws = self.active_ws().unwrap();
        let visible = self.visible_tab_ids(ws, ws.group_of(tab), cx);
        if !visible.contains(tab)
            || before.is_some_and(|b| !self.visible_tab_ids(ws, to, cx).contains(b))
        {
            return;
        }
        // A file edited inline in Diff can't be selected as an editor tab.
        if let TabId::File(path) = tab
            && let Err(error) = self.check_diff_edits(path, cx)
        {
            return notify_err(window, cx, error);
        }
        if !self.active_ws_mut().unwrap().move_to_group(tab, to, before) {
            return;
        }
        self.sort_file_tabs(cx);
        let _ = self.state.save();
        self.select_id(tab.clone(), window, cx);
        self.prepare_other_group(window, cx);
        self.reveal_tab(cx);
        if let Some(name) = self.active.clone() {
            self.record_interaction(&name, cx);
        }
        cx.notify();
    }

    /// cmd-\: the focused tab to the other group; with one group, into a new right group.
    pub(super) fn move_tab_to_other_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.home_shown() {
            return;
        }
        let (Some(name), Some(tab)) = (self.active.clone(), self.shown_id(cx)) else {
            return notify_err(window, cx, "Open a workspace first");
        };
        let ws = self.active_ws().unwrap();
        let to = match ws.split() {
            true => 1 - ws.group_of(&tab),
            false => 1,
        };
        if !self.can_move_to(&tab, to, cx) {
            return notify_err(
                window,
                cx,
                "Open another tab first; a group can't be left empty",
            );
        }
        self.move_to_group(&name, &tab, to, None, window, cx);
    }

    /// The palette offers "Move tab to other group" only when it would do something.
    pub(super) fn can_move_shown_tab(&self, cx: &App) -> bool {
        let Some(ws) = self.active_ws().filter(|_| !self.home_shown()) else {
            return false;
        };
        let Some(tab) = self.shown_id(cx) else {
            return false;
        };
        let to = if ws.split() { 1 - ws.group_of(&tab) } else { 1 };
        self.can_move_to(&tab, to, cx)
    }

    /// The divider's state for the open workspace, made on first split; the saved ratio is
    /// applied once the container's width is known, and drags are saved back.
    pub(super) fn prepare_split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.active_ws() else { return };
        let name = ws.name.clone();
        if !ws.split() {
            // The next split starts at half and half.
            self.splits.remove(&name);
            return;
        }
        let ratio = ws.split_ratio();
        if !self.splits.contains_key(&name) {
            let state = cx.new(|_| ResizableState::default());
            let ws_name = name.clone();
            self._subs.push(
                cx.subscribe(&state, move |s, r, _: &ResizablePanelEvent, cx| {
                    let sizes = r.read(cx).sizes().clone();
                    let total: f32 = sizes.iter().map(|p| p.as_f32()).sum();
                    if sizes.len() != 2 || total <= 0. {
                        return;
                    }
                    let ratio = sizes[0].as_f32() / total;
                    if let Some(ws) = s.ws_mut(&ws_name)
                        && ws.layout.ratio.is_none_or(|r| (r - ratio).abs() > 0.001)
                    {
                        ws.layout.ratio = Some(ratio);
                        let _ = s.state.save();
                    }
                }),
            );
            self.splits.insert(name.clone(), (state, false));
        }
        let (state, applied) = self.splits.get_mut(&name).unwrap();
        let (container, sizes) = {
            let s = state.read(cx);
            (s.container_size(), s.sizes().clone())
        };
        if !*applied && container > px(0.) && sizes.len() == 2 {
            *applied = true;
            let target = container * ratio;
            if (sizes[0] - target).abs() > px(1.) {
                state.update(cx, |s, cx| s.resize_panel(0, target, window, cx));
            }
        }
    }
}

/// What a group shows when its selection has no content yet: a file still being read, or one
/// that couldn't be.
pub(super) fn placeholder(g: usize, text: String, theme: &Theme) -> AnyElement {
    div()
        .id(("group-placeholder", g))
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(theme.muted_foreground)
        .aria_label(text.clone())
        .child(text)
        .test_support()
        .into_any_element()
}
