//! Chat tabs of the open workspace, and the agent status in the header.
use super::*;

/// Resolved entries in the persisted strip; unavailable files have no visible tab.
struct VisibleTabs(Vec<(TabId, VisibleTab)>);

#[derive(Clone, Copy, Debug, PartialEq)]
enum VisibleTab {
    Chat(usize),
    Feature(Feature),
    File(usize),
}

impl VisibleTabs {
    fn len(&self) -> usize {
        self.0.len()
    }
    fn get(&self, ix: usize) -> Option<VisibleTab> {
        self.0.get(ix).map(|(_, t)| *t)
    }
    fn index(&self, tab: VisibleTab) -> Option<usize> {
        self.0.iter().position(|(_, t)| *t == tab)
    }
}

/// The accent dot on a tab: a finished turn you haven't looked at yet.
pub(super) fn dot(inset: f32, theme: &Theme) -> Div {
    (div().absolute().top(px(inset)).right(px(inset)))
        .size(px(6.))
        .rounded_full()
        .bg(theme.primary)
}

impl Shark {
    /// Group `g`'s strip.
    fn visible_tabs(&self, ws: &Workspace, g: usize, cx: &App) -> VisibleTabs {
        VisibleTabs(
            ws.tab_order
                .iter()
                .filter(|id| ws.group_of(id) == g)
                .filter_map(|id| {
                    let tab = match id {
                        TabId::Chat(id) => {
                            VisibleTab::Chat(ws.chats.iter().position(|c| &c.id == id)?)
                        }
                        TabId::Diff if !ws.diff_closed => VisibleTab::Feature(Feature::Diff),
                        TabId::Git if !ws.git_closed => VisibleTab::Feature(Feature::Git),
                        TabId::File(path) => VisibleTab::File(
                            self.file_tabs
                                .iter()
                                .position(|t| &t.editor.read(cx).path == path)?,
                        ),
                        _ => return None,
                    };
                    Some((id.clone(), tab))
                })
                .collect(),
        )
    }

    /// Where group `g`'s selection sits in its strip.
    pub(super) fn active_tab_index(&self, g: usize, cx: &App) -> Option<usize> {
        let ws = self.active_ws()?;
        let sel = self.group_selection(g, cx)?;
        (self.visible_tabs(ws, g, cx).0.iter()).position(|(id, _)| *id == sel)
    }

    /// The chat entity for `meta`, started (transcript replayed) on first use.
    pub(super) fn ensure_chat(
        &mut self,
        ws: &Workspace,
        meta: &ChatMeta,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<Chat> {
        if let Some(chat) = self.chats.get(&meta.id) {
            return chat.clone();
        }
        // Opened mid-archive (its row is still listed): sends stay refused.
        let archiving = self.archiving.contains(&ws.name);
        let vim = self.state.vim;
        let chat = cx.new(|cx| {
            let mut c = Chat::new(ws.clone(), meta.clone(), vim, window, cx);
            c.archiving = archiving;
            c
        });
        let (id, name) = (meta.id.clone(), ws.name.clone());
        // Busy also flips without a ChatEvent (background tasks, turns the CLI starts itself).
        self._subs.push(cx.observe(&chat, |this, chat, cx| {
            this.update_awake(cx);
            // Home's "Approve a plan": approving notifies the chat.
            if !this.state.approved_plan && chat.read(cx).plan.read(cx).approved.is_some() {
                this.state.mark(|s| &mut s.approved_plan);
            }
            cx.notify()
        }));
        self._subs.push(cx.subscribe_in(
            &chat,
            window,
            move |this, chat, ev: &ChatEvent, window, cx| {
                match ev {
                    ChatEvent::Meta => this.save_meta(chat, cx),
                    ChatEvent::GitReviewsSent(_) => {} // GitSync records these.
                    ChatEvent::Picked => {
                        this.record_interaction(&name, cx);
                        // The level the chat runs at, so an old chat with none doesn't clear it.
                        let m = &chat.read(cx).meta;
                        let pick = (m.model.clone(), Some(crate::chat::effort_of(m).to_string()));
                        let st = &mut this.state;
                        if (&st.last_model, &st.last_effort) != (&pick.0, &pick.1) {
                            (st.last_model, st.last_effort) = pick;
                            let _ = st.save();
                        }
                    }
                    ChatEvent::TurnDone => {
                        if this.active.as_ref() == Some(&name) {
                            this.refresh_diff(cx);
                        }
                        if !this.chat_on_screen(chat, cx) {
                            this.unread.insert(id.clone());
                        }
                        this.refresh_home(Some(&name), cx);
                        this.turn_done(&name, cx);
                        if let Some(ws) = this.ws(&name) {
                            let dir = ws.dir();
                            let c = chat.read(cx);
                            let (turn, reply, held) =
                                (c.last_turn, c.last_reply.clone(), c.unread_review_ids(true));
                            this.differ.update(cx, |d, cx| {
                                d.review_turn_done(&name, &dir, &id, turn, &reply, &held, cx)
                            });
                        }
                        cx.notify();
                    }
                    // A question off screen marks its tab; the workspace needs you until it's answered.
                    ChatEvent::Asking => {
                        if !this.chat_on_screen(chat, cx) && chat.read(cx).asking() {
                            this.unread.insert(id.clone());
                        }
                        this.refresh_home(Some(&name), cx);
                        this.refresh_attention(cx);
                    }
                    ChatEvent::ShowChanges | ChatEvent::ShowComment(_) => {
                        if this.active.as_ref() == Some(&name) {
                            this.show(Tab::Feature(Feature::Diff), window, cx);
                            if let ChatEvent::ShowComment(id) = ev {
                                this.differ
                                    .update(cx, |d, cx| d.jump_to_comment(*id, window, cx));
                            }
                        }
                    }
                    ChatEvent::ReviewRemoved(ids) => this
                        .differ
                        .update(cx, |d, cx| d.review_delivered(&name, &id, ids, None, cx)),
                    ChatEvent::ReviewSent { ids, turn } => this.differ.update(cx, |d, cx| {
                        d.review_delivered(&name, &id, ids, Some(*turn), cx)
                    }),
                }
            },
        ));
        if let Some(sync) = self.syncs.get(&ws.name) {
            sync.update(cx, |g, cx| g.add_chat(chat.clone(), cx));
        }
        self.chats.insert(meta.id.clone(), chat.clone());
        self.save_meta(&chat, cx); // replay may have found a title
        chat
    }

    fn save_meta(&mut self, chat: &Entity<Chat>, cx: &mut Context<Self>) {
        let meta = chat.read(cx).meta.clone();
        let slot = (self.state.workspaces.iter_mut())
            .flat_map(|w| w.chats.iter_mut())
            .find(|c| c.id == meta.id);
        if let Some(slot) = slot
            && *slot != meta
        {
            *slot = meta;
            let _ = self.state.save();
        }
    }

    /// Shows the workspace's `ix`th chat tab.
    pub(super) fn select_chat(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.active_ws_mut() else {
            return;
        };
        let Some(meta) = ws.chats.get(ix).cloned() else {
            return;
        };
        if ws.active_chat != ix {
            ws.active_chat = ix;
            let _ = self.state.save();
        }
        let ws = self.active_ws().cloned().unwrap();
        let chat = self.ensure_chat(&ws, &meta, window, cx);
        self.unread.remove(&meta.id);
        if let Some(sync) = self.syncs.get(&ws.name) {
            sync.update(cx, |g, cx| g.set_chat(chat.clone(), cx));
        }
        self.show(Tab::Chat, window, cx);
        chat.update(cx, |c, cx| c.focus_composer(window, cx));
        cx.notify();
    }

    /// Reorder identities, keeping entities, selected content and focus intact.
    pub(super) fn move_tab(
        &mut self,
        workspace: &str,
        from: &TabId,
        to: &TabId,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.active_ws().filter(|ws| ws.name == workspace) else {
            return;
        };
        // Within one strip; between strips is `move_to_group`.
        let g = ws.group_of(from);
        let tabs = self.visible_tabs(ws, g, cx);
        if ![from, to]
            .iter()
            .all(|id| tabs.0.iter().any(|(t, _)| t == *id))
        {
            return;
        }
        if !self.active_ws_mut().unwrap().move_tab(from, to) {
            return;
        }
        self.sort_file_tabs(cx);
        let _ = self.state.save();
        self.reveal_tab(cx);
        cx.notify();
    }

    pub(super) fn tab_drag(
        &self,
        pill: Stateful<Div>,
        id: TabId,
        label: SharedString,
        _: &Context<Self>,
    ) -> Stateful<Div> {
        self.reorder_source(
            pill.on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default()),
            reorder::Item::Tab(self.active.clone().unwrap(), id),
            label,
        )
    }

    /// Group `g`'s strip, as identities.
    pub(super) fn visible_tab_ids(&self, ws: &Workspace, g: usize, cx: &App) -> Vec<TabId> {
        self.visible_tabs(ws, g, cx)
            .0
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    }

    pub(super) fn reveal_tab(&self, cx: &App) {
        for g in 0..2 {
            if let Some(active) = self.active_tab_index(g, cx) {
                self.tab_scroll[g].scroll_to_item(active);
            }
        }
    }

    /// cmd-shift-] and [: the focused group's chats; nothing when it has none.
    pub(super) fn cycle_chat(&mut self, d: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.active_ws() else { return };
        let g = ws.layout.focused;
        let chats: Vec<usize> = (ws.chats.iter().enumerate())
            .filter(|(_, c)| ws.group_of(&TabId::Chat(c.id.clone())) == g)
            .map(|(ix, _)| ix)
            .collect();
        if chats.is_empty() {
            return;
        }
        let n = chats.len() as isize;
        let next = match chats.iter().position(|&ix| ix == ws.active_chat) {
            Some(at) => (at as isize + d).rem_euclid(n),
            None if d > 0 => 0,
            None => n - 1,
        };
        self.select_chat(chats[next as usize], window, cx);
    }

    /// Ctrl-tab follows the focused group's strip: its part of the saved mixed tab order.
    pub(super) fn cycle_tab(&mut self, d: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.home_shown() {
            return self.cycle_chat(d, window, cx);
        }
        let Some(ws) = self.active_ws() else { return };
        let g = ws.layout.focused;
        let Some(active) = self.active_tab_index(g, cx) else {
            return;
        };
        let n = self.visible_tabs(ws, g, cx).len() as isize;
        let next = (active as isize + d).rem_euclid(n) as usize;
        self.select_tab(next, window, cx);
    }

    /// ctrl-1…9: the nth tab of the focused group.
    pub(super) fn select_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.active_ws() else { return };
        match self.visible_tabs(ws, ws.layout.focused, cx).get(ix) {
            Some(VisibleTab::Chat(ix)) => self.select_chat(ix, window, cx),
            Some(VisibleTab::Feature(feature)) => self.show(Tab::Feature(feature), window, cx),
            Some(VisibleTab::File(ix)) => self.select_file(ix, window, cx),
            None => {}
        }
    }

    /// cmd-t: a new tab at the end of the strip, on the last picked model and thinking level
    /// and the current chat's speed.
    pub(super) fn new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let fast = self.active_chat().and_then(|c| c.read(cx).meta.fast);
        let Some(id) = self.active_ws().map(Workspace::next_chat_id) else {
            return notify_err(window, cx, "Open a workspace first");
        };
        let meta = ChatMeta {
            fast,
            ..self.state.new_chat(id)
        };
        let ws = self.active_ws_mut().unwrap();
        let at = ws.chats.len();
        ws.open_tab(TabId::Chat(meta.id.clone()));
        ws.chats.push(meta);
        let _ = self.state.save();
        self.select_chat(at, window, cx);
    }

    pub(super) fn close_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.home_shown() {
            return;
        }
        match self.tab {
            Tab::File => self.close_file(window, cx),
            Tab::Feature(feature) => self.close_feature(feature, window, cx),
            Tab::Chat => self.close_chat(window, cx),
        }
    }

    /// Closes the shown chat, asking first if it's running. The last tab gives way to a
    /// fresh one, so a workspace always has a chat.
    pub(super) fn close_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The chat isn't on screen.
        if self.home_shown() {
            return;
        }
        let Some(ws) = self.active_ws() else { return };
        let (name, id) = (ws.name.clone(), ws.chats[ws.active_chat].id.clone());
        self.close_chat_by_id(&name, &id, window, cx);
    }

    fn close_chat_by_id(
        &mut self,
        workspace: &str,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chat = self.chats.get(id).filter(|chat| chat.read(cx).working());
        let Some(chat) = chat else {
            return self.close_chat_now(workspace, id, window, cx);
        };
        let title = chat.read(cx).title();
        let (workspace, id) = (workspace.to_string(), id.to_string());
        confirm(
            format!("Stop and close “{title}”?"),
            "The agent is still running in this chat. Closing stops it; cmd-shift-t reopens the chat.",
            "Stop and close",
            move |s, w, cx| s.close_chat_now(&workspace, &id, w, cx),
            window,
            cx,
        );
    }

    fn close_chat_now(
        &mut self,
        workspace: &str,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(chat) = self.chats.get(id) {
            chat.update(cx, |c, cx| c.retry_history(cx));
            if chat.read(cx).history_save_error.is_some() {
                return notify_err(
                    window,
                    cx,
                    "Answer sent, but could not save it to history. Retry save before closing this chat.",
                );
            }
        }
        let fresh = self.state.new_chat(String::new());
        let Some(ws) = self
            .state
            .workspaces
            .iter_mut()
            .find(|ws| ws.name == workspace)
        else {
            return;
        };
        let Some(ix) = ws.chats.iter().position(|c| c.id == id) else {
            return;
        };
        let selected = ws.chats[ws.active_chat].id.clone();
        let fresh_id = ws.next_chat_id();
        let split = ws.split();
        let closed = TabId::Chat(id.to_string());
        let meta = ws.chats.remove(ix);
        if ws.chats.is_empty() {
            // The fresh chat takes the last one's place, in its group.
            ws.replace_tab(&closed, TabId::Chat(fresh_id.clone()));
            ws.chats.push(ChatMeta {
                id: fresh_id,
                ..fresh
            });
        } else {
            ws.close_tab(&closed);
        }
        ws.active_chat = ws
            .chats
            .iter()
            .position(|c| c.id == selected)
            .unwrap_or_else(|| ix.min(ws.chats.len() - 1));
        ws.normalize_tabs();
        let next = ws.active_chat;
        // No session can also mean one just dropped (a backend switch): the transcript tells.
        let untouched =
            meta.session_id.is_none() && !crate::agent::transcript_path(&meta.id).exists();
        let name = ws.name.clone();
        // An empty chat isn't worth reopening.
        if !untouched {
            ws.closed.push(meta.clone());
        }
        // ponytail: keeps the 20 newest closed chats; a closed-chats browser can lift this
        let drop = ws.closed.len().saturating_sub(20);
        let evicted: Vec<ChatMeta> = ws.closed.drain(..drop).collect();
        let _ = self.state.save();
        for m in evicted.iter().chain(untouched.then_some(&meta)) {
            let _ = std::fs::remove_file(crate::agent::transcript_path(&m.id));
            crate::plan::delete(&m.id);
        }
        self.unread.remove(&meta.id);
        // Its queue goes with it, and claude with what it hadn't read: those are pending again.
        let unread =
            (self.chats.get(&meta.id)).map_or(vec![], |c| c.read(cx).unread_review_ids(false));
        self.differ
            .update(cx, |d, cx| d.unqueue_review(&name, &meta.id, &unread, cx));
        if let Some(chat) = self.chats.remove(&meta.id)
            && let Some(sync) = self.syncs.get(&name)
        {
            sync.update(cx, |g, cx| g.remove_chat(&chat, cx)); // the last ref; dropping stops claude
        }
        let active = self.active.as_deref() == Some(workspace);
        if active && split {
            let shown = self.tab == Tab::Chat && selected == id;
            if shown && !self.home_shown() {
                self.sync_groups(true, window, cx);
            } else {
                // Its group shows a neighbor; the focused chat may have changed.
                let ws = self.active_ws().cloned().unwrap();
                let chat = self.ensure_chat(&ws, &ws.chats[ws.active_chat], window, cx);
                if let Some(sync) = self.syncs.get(workspace) {
                    sync.update(cx, |g, cx| g.set_chat(chat, cx));
                }
                self.sync_groups(false, window, cx);
            }
        } else if active && selected == id {
            if self.tab == Tab::Chat && !self.home_shown() {
                self.select_chat(next, window, cx);
            } else {
                let ws = self.active_ws().cloned().unwrap();
                let chat = self.ensure_chat(&ws, &ws.chats[next], window, cx);
                if let Some(sync) = self.syncs.get(workspace) {
                    sync.update(cx, |g, cx| g.set_chat(chat, cx));
                }
            }
        }
        cx.notify();
    }

    pub(super) fn close_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.active.clone() else {
            return;
        };
        self.confirm_discard(false, true, window, cx, move |s, w, cx| {
            if s.active.as_ref() != Some(&name) {
                return;
            }
            s.differ.update(cx, |d, cx| d.end_edit(cx));
            let ws = s.active_ws_mut().unwrap();
            let split = ws.split();
            ws.diff_closed = true;
            ws.close_tab(&TabId::Diff);
            let _ = s.state.save();
            let shown = s.tab == Tab::Feature(Feature::Diff);
            if split {
                s.sync_groups(shown, w, cx);
            } else if shown {
                let ix = s.active_ws().unwrap().active_chat;
                s.select_chat(ix, w, cx);
            }
            cx.notify();
        });
    }

    /// cmd-shift-t: appends the most recently closed chat, or a picked one, at the end of the strip.
    pub(super) fn reopen_chat(
        &mut self,
        pick: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.active_ws_mut() else {
            return notify_err(window, cx, "Open a workspace first");
        };
        let Some(ix) = pick.or(ws.closed.len().checked_sub(1)) else {
            return notify_err(window, cx, "No closed chats in this workspace");
        };
        let meta = ws.closed.remove(ix);
        let at = ws.chats.len();
        ws.open_tab(TabId::Chat(meta.id.clone()));
        ws.chats.push(meta);
        let _ = self.state.save();
        self.select_chat(at, window, cx);
        if let Some(name) = self.active.clone() {
            self.record_interaction(&name, cx);
        }
    }

    pub(super) fn pick_closed_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.active_ws() else {
            return notify_err(window, cx, "Open a workspace first");
        };
        if ws.closed.is_empty() {
            return notify_err(window, cx, "No closed chats in this workspace");
        }
        // Newest first.
        let n = ws.closed.len();
        let items = (ws.closed.iter().rev())
            .map(|c| {
                let title = if c.title.is_empty() {
                    "Untitled chat"
                } else {
                    &c.title
                };
                CommandItem::new()
                    .label(title.to_string())
                    .icon(IconName::MessageSquare)
            })
            .collect();
        open_menu(
            cx.entity().downgrade(),
            "Reopen a closed chat…",
            items,
            0,
            move |s, row, w, cx| s.reopen_chat(Some(n - 1 - row), w, cx),
            window,
            cx,
        );
    }

    pub(super) fn open_side(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = self.active_chat().cloned() else {
            return notify_err(window, cx, "Open a workspace first");
        };
        self.show(Tab::Chat, window, cx);
        chat.update(cx, |c, cx| c.open_side(window, cx));
        cx.notify();
    }

    pub(super) fn focus_queue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(chat) = self.active_chat().cloned() {
            self.show(Tab::Chat, window, cx);
            chat.update(cx, |c, cx| c.focus_queue(window, cx));
        }
    }

    /// How many of a workspace's chats have something going on.
    /// Chats whose agent is at work. One waiting on answers to its questions isn't: it
    /// needs you (`asking_chat`).
    pub(crate) fn running_chats(&self, ws: &str, cx: &App) -> usize {
        (self.started_chats(ws).iter())
            .filter(|c| c.read(cx).working() && !c.read(cx).asking())
            .count()
    }

    pub(crate) fn asking_chat(&self, ws: &str, cx: &App) -> bool {
        (self.started_chats(ws).iter()).any(|c| c.read(cx).asking())
    }

    /// A workspace's open chat tabs that have been shown (and so have an entity).
    pub(super) fn started_chats(&self, ws: &str) -> Vec<Entity<Chat>> {
        let Some(ws) = self.ws(ws) else {
            return vec![];
        };
        (ws.chats.iter())
            .filter_map(|c| self.chats.get(&c.id).cloned())
            .collect()
    }

    /// A chat tab of the workspace finished a turn that hasn't been looked at.
    pub(crate) fn chat_unread(&self, ws: &str) -> bool {
        self.ws(ws)
            .is_some_and(|w| w.chats.iter().any(|c| self.unread.contains(&c.id)))
    }

    pub(super) fn stop(&mut self, cx: &mut Context<Self>) {
        if self.home_shown() {
            self.cancel_ask(cx); // ctrl-c on Home stops Ask Shark, not the chat behind it
        } else if let Some(chat) = self.active_chat() {
            chat.update(cx, |c, cx| c.stop(cx));
        }
    }

    pub(super) fn pick_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = self.active_chat().cloned() else {
            return notify_err(window, cx, "Open a workspace first");
        };
        chat.update(cx, |c, cx| c.open_model_menu(window, cx));
    }

    /// Plan commands act on the shown chat, and bring its tab up.
    pub(super) fn with_plan(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Chat, &mut Window, &mut Context<Chat>),
    ) {
        let Some(chat) = self.active_chat().cloned() else {
            return notify_err(window, cx, "Open a workspace first");
        };
        if self.tab != Tab::Chat || self.home.on {
            self.show(Tab::Chat, window, cx);
        }
        chat.update(cx, |c, cx| f(c, window, cx));
    }

    /// Fast on or off in the shown chat, toasting the result: a shortcut has no button in view.
    pub(super) fn toggle_fast(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = self.active_chat().cloned() else {
            return notify_err(window, cx, "Open a workspace first");
        };
        if !chat.read(cx).codex() {
            return notify_err(window, cx, "Fast mode needs a Codex model");
        }
        let on = !chat.read(cx).fast();
        chat.update(cx, |c, cx| c.set_fast(on, cx));
        window.push_notification(if on { "Fast mode on" } else { "Fast mode off" }, cx);
    }

    /// Approves the shown chat's plan and builds it in a new chat that gets only the plan, so
    /// the planning back-and-forth stays behind.
    pub(super) fn build_plan_in_new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(from) = self.active_chat().cloned() else {
            return notify_err(window, cx, "Open a workspace first");
        };
        let Some(plan) = (from.read(cx).plan.read(cx).latest()).map(|v| v.text.clone()) else {
            return notify_err(
                window,
                cx,
                "No plan yet; the agent submits one in plan mode",
            );
        };
        self.new_chat(window, cx);
        let Some(to) = self.active_chat().cloned().filter(|c| *c != from) else {
            return;
        };
        if self.tab != Tab::Chat {
            self.show(Tab::Chat, window, cx);
        }
        if to.update(cx, |c, cx| c.build_plan(&plan, window, cx)) {
            from.update(cx, |c, cx| c.plan_approved(cx));
        }
    }

    /// The no-menu fast path, so confirm the switch in a toast.
    pub(super) fn set_model(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(chat), Some(m)) = (self.active_chat().cloned(), MODELS.get(ix)) else {
            return notify_err(window, cx, "Open a workspace first");
        };
        chat.update(cx, |c, cx| c.set_model(m, cx));
        let m = crate::chat::model_label(m);
        window.push_notification(format!("Model: {m} (from the next message)"), cx);
    }

    /// Palette: the shown chat's thinking level, confirmed in a toast.
    pub(super) fn set_effort(&mut self, level: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = self.active_chat().cloned() else {
            return notify_err(window, cx, "Open a workspace first");
        };
        chat.update(cx, |c, cx| c.set_effort(level, cx));
        self.effort_toast(&chat, window, cx);
    }

    /// cmd-shift-. / cmd-shift-,: the shown chat's thinking level one up or down.
    pub(super) fn step_effort(&mut self, d: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = self.active_chat().cloned() else {
            return notify_err(window, cx, "Open a workspace first");
        };
        chat.update(cx, |c, cx| c.step_effort(d, cx));
        self.effort_toast(&chat, window, cx);
    }

    fn effort_toast(&self, chat: &Entity<Chat>, window: &mut Window, cx: &mut Context<Self>) {
        let l = crate::chat::effort_label(chat.read(cx).effort());
        window.push_notification(format!("Thinking: {l} (from the next message)"), cx);
    }

    /// Where tab `ix` of group `g` sits against its strip as of the last layout: `Less` hidden
    /// (partly counts) past the left edge, `Greater` past the right, `Equal` in view.
    pub(super) fn tab_hidden(&self, g: usize, ix: usize) -> Ordering {
        let scroll = &self.tab_scroll[g];
        let Some(b) = scroll.bounds_for_item(ix) else {
            return Ordering::Equal;
        };
        let (view, x) = (scroll.bounds(), scroll.offset().x);
        if b.left() + x < view.left() - px(1.) {
            Ordering::Less
        } else if b.right() + x > view.right() + px(1.) {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }

    /// An arrow at an edge of group `g`'s strip while tabs hide past it, dotted when one of them
    /// finished a turn unseen; clicking brings that one (else the nearest hidden tab) into view.
    fn tab_edge(
        &self,
        g: usize,
        edge: Ordering,
        ws: &Workspace,
        tabs: &VisibleTabs,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let mut hidden: Vec<usize> = (0..tabs.len())
            .filter(|&ix| self.tab_hidden(g, ix) == edge)
            .collect();
        if edge == Ordering::Less {
            hidden.reverse(); // nearest first
        }
        let unread: Vec<usize> = (hidden.iter().copied())
            .filter(|&ix| match tabs.get(ix) {
                Some(VisibleTab::Chat(ix)) => self.unread.contains(&ws.chats[ix].id),
                _ => false,
            })
            .collect();
        let to = *unread.first().or(hidden.first())?;
        let theme = cx.theme();
        let (id, icon) = match edge {
            Ordering::Less => ("tabs-left", IconName::ChevronLeft),
            _ => ("tabs-right", IconName::ChevronRight),
        };
        let id = match g {
            0 => SharedString::from(id),
            _ => format!("{id}-{g}").into(),
        };
        let tip = match unread.len() {
            0 => format!("{} more", hidden.len()),
            n => format!("{} more · {n} finished", hidden.len()),
        };
        Some(
            div()
                .relative()
                .flex_none()
                .child(
                    Button::new(id)
                        .ghost()
                        .xsmall()
                        .icon(icon)
                        .tooltip(tip)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.tab_scroll[g].scroll_to_item(to);
                            cx.on_next_frame(window, |_, _, cx| cx.notify());
                            cx.notify();
                        })),
                )
                .when(!unread.is_empty(), |d| d.child(dot(2., theme)))
                .into_any_element(),
        )
    }

    /// The titlebar's tab area: one strip per group, each with its new-chat button and
    /// trailing space, aligned with the group below. Window dragging owns the trailing space
    /// except while a tab drags, when it takes drops; the last one offers a split.
    pub(super) fn render_strips(&self, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(ws) = self.active_ws() else {
            return vec![];
        };
        let split = self.split_shown();
        let groups = if split { 2 } else { 1 };
        // While split, each strip spans its island below (content plus its 1px border), with
        // the same gap between them.
        let (left_width, gap) = {
            let (area, bounds) = (self.tabs_area.get(), self.group_bounds.get());
            let measured = split && !area.size.width.is_zero() && !bounds[1].size.width.is_zero();
            let left = bounds[0].right() + px(1.);
            match measured {
                true => (
                    Some((left - area.left()).max(px(0.))),
                    (bounds[1].left() - px(1.) - left).max(px(0.)),
                ),
                false => (None, px(0.)),
            }
        };
        let reorder = self.reorder.borrow();
        let dragging =
            cx.has_active_drag() && matches!(reorder.source, Some(reorder::Item::Tab(..)));
        let source_group = match &reorder.source {
            Some(reorder::Item::Tab(_, id)) => Some(ws.group_of(id)),
            _ => None,
        };
        let target = reorder.target.clone();
        drop(reorder);
        let drop_group = match (&target, source_group) {
            (Some(reorder::Item::Append(_, g)), _) => Some(*g),
            (Some(reorder::Item::Tab(_, id)), Some(from)) if ws.group_of(id) != from => {
                Some(ws.group_of(id))
            }
            _ => None,
        };
        let split_target = dragging && !split && self.visible_tab_ids(ws, 0, cx).len() > 1;
        let theme = cx.theme();
        let (accent, faint, radius) = (
            theme.primary.opacity(0.06),
            theme.primary.opacity(0.16),
            theme.radius,
        );
        let name = ws.name.clone();
        (0..groups)
            .map(|g| {
                let last = g + 1 == groups;
                let empty = if last {
                    "titlebar-empty"
                } else {
                    "titlebar-gap"
                };
                let trailing = div()
                    .id(empty)
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .when(dragging, |d| {
                        // A drop zone even when the tabs overflow.
                        d.min_w(px(32.)).when(
                            target == Some(reorder::Item::Append(name.clone(), g)),
                            |d| d.bg(accent).rounded(radius),
                        )
                    })
                    .when(last && split_target, |d| {
                        // Room for the target even when the tabs overflow.
                        d.min_w(px(48.)).child(self.render_split_target(
                            target == Some(reorder::Item::Split(name.clone())),
                            cx,
                        ))
                    })
                    .map(|d| self.draggable_chrome(empty, d));
                let trailing = self.reorder_item(
                    trailing.into_any_element(),
                    reorder::Item::Append(name.clone(), g),
                );
                h_flex()
                    .h_full()
                    .min_w_0()
                    .map(|d| match (last, left_width) {
                        (false, Some(w)) => d.flex_none().w(w),
                        _ => d.flex_1(),
                    })
                    .when(split && g == 1, |d| d.pl(gap))
                    .child(
                        h_flex()
                            .min_w_0()
                            .h_full()
                            .gap(px(4.))
                            .rounded(radius)
                            .when(drop_group == Some(g), |d| d.bg(faint))
                            .children(self.render_tabs(g, window, cx))
                            .child(
                                Button::new(match g {
                                    0 => SharedString::from("new-chat"),
                                    _ => format!("new-chat-{g}").into(),
                                })
                                .ghost()
                                .xsmall()
                                .icon(IconName::Plus)
                                .w(px(24.))
                                .flex_none()
                                .tooltip("New chat (cmd-t)")
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.adopt_group(g, window, cx);
                                        this.new_chat(window, cx)
                                    },
                                )),
                            ),
                    )
                    .child(trailing)
                    .into_any_element()
            })
            .collect()
    }

    /// Both groups show their selection; a bar under it marks the focused group's.
    pub(super) fn focus_mark(
        &self,
        g: usize,
        on: bool,
        d: Stateful<Div>,
        theme: &Theme,
    ) -> Stateful<Div> {
        d.when(on && self.split_shown() && g == self.focused_group(), |d| {
            d.child(
                div()
                    .id("focused-group-mark")
                    .absolute()
                    .bottom_0()
                    .left(px(8.))
                    .right(px(8.))
                    .h(px(2.))
                    .rounded_full()
                    .bg(theme.primary)
                    .test_support(),
            )
        })
    }

    /// The dotted drop target that splits the workspace, shown while a tab drags.
    fn render_split_target(&self, hovered: bool, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let target = h_flex()
            .id("split-target")
            .role(Role::Button)
            .aria_label("Drop to open in a new right group")
            .flex_none()
            .ml(px(4.))
            .w(px(36.))
            .h(px(28.))
            .justify_center()
            .rounded(theme.radius)
            .border_1()
            .border_dashed()
            .border_color(theme.primary.opacity(if hovered { 0.9 } else { 0.5 }))
            .bg(theme.primary.opacity(if hovered { 0.18 } else { 0.06 }))
            .text_color(theme.primary)
            .child(Icon::new(IconName::Columns2).with_size(px(14.)))
            .test_support()
            .into_any_element();
        let name = self.active.clone().unwrap_or_default();
        self.reorder_item(target, reorder::Item::Split(name))
    }

    /// Group `g`'s tabs: chats, features and files in one equal-width strip that shrinks to fit.
    pub(super) fn render_tabs(
        &self,
        g: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(ws) = self.active_ws() else {
            return vec![];
        };
        let theme = cx.theme();
        let tabs = self.visible_tabs(ws, g, cx);
        let split = self.split_shown();
        let focused = g == ws.layout.focused;
        let sel = self.group_selection(g, cx);
        let m = window.modifiers();
        // Numbers only on the group they act on.
        let ctrl_held = m.control && m.number_of_modifiers() == 1 && focused;
        let mut pills: Vec<(VisibleTab, AnyElement)> = (ws.chats.iter().enumerate())
            .filter_map(|(ix, meta)| {
                let tab_ix = tabs.index(VisibleTab::Chat(ix))?;
                let chat = self.chats.get(&meta.id).map(|c| c.read(cx));
                let on = sel.as_ref() == Some(&TabId::Chat(meta.id.clone()));
                let label: SharedString = match chat {
                    Some(c) => c.title(),
                    None if meta.title.is_empty() => "New chat".into(),
                    None => meta.title.clone().into(),
                };
                // Comments of a review the agent hasn't read yet.
                let label = match chat.map_or(0, |c| c.review_count()) {
                    0 => label,
                    n => format!("{label} · {n}").into(),
                };
                let icon = match (
                    chat.is_some_and(|c| c.working()),
                    self.unread.contains(&meta.id),
                ) {
                    _ if ctrl_held && tab_ix < 9 => {
                        kbd(&format!("ctrl-{}", tab_ix + 1)).into_any_element()
                    }
                    // Waiting on answers to its questions.
                    _ if chat.is_some_and(|c| c.asking()) => {
                        Icon::new(IconName::MessageCircleQuestionMark)
                            .with_size(px(14.))
                            .text_color(theme.warning_foreground)
                            .into_any_element()
                    }
                    (true, _) => Spinner::new()
                        .icon(IconName::LoaderCircle)
                        .color(theme.primary)
                        .with_size(px(14.))
                        .into_any_element(),
                    (false, unread) => div()
                        .relative()
                        .child(Icon::new(IconName::MessageSquare).with_size(px(14.)))
                        .when(unread, |d| d.child(dot(-2., theme)))
                        .into_any_element(),
                };
                Some((
                    VisibleTab::Chat(ix),
                    pill(format!("chat-tab-{}", meta.id), on, theme)
                        .aria_label(label.clone())
                        .pl_2p5()
                        .pr_1()
                        .map(|d| self.tab_drag(d, TabId::Chat(meta.id.clone()), label.clone(), cx))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.select_chat(ix, window, cx);
                        }))
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, e: &MouseDownEvent, w, cx| {
                                this.select_chat(ix, w, cx);
                                let entries = this.chat_tab_menu(ix, cx);
                                this.show_menu(e.position, entries, w, cx);
                            }),
                        )
                        .child(icon)
                        .child(
                            tab_label(label.clone(), on, theme)
                                .id("tab-label")
                                .test_support(),
                        )
                        .tooltip(move |w, cx| Tooltip::new(label.clone()).build(w, cx))
                        .child(tab_close(
                            crate::settings::close_button("close-chat", "Close chat (cmd-w)")
                                .on_click({
                                    let (workspace, id) = (ws.name.clone(), meta.id.clone());
                                    cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.close_chat_by_id(&workspace, &id, window, cx)
                                    })
                                }),
                            theme,
                        ))
                        .map(|d| self.focus_mark(g, on, d, theme))
                        .test_support()
                        .into_any_element(),
                ))
            })
            .collect();
        for feature in Feature::ALL {
            if let Some(ix) = tabs.index(VisibleTab::Feature(feature)) {
                let on = sel.as_ref() == Some(&feature.tab_id());
                pills.push((
                    VisibleTab::Feature(feature),
                    self.render_feature_tab(feature, ix, on, ctrl_held, cx),
                ));
            }
        }
        pills.extend(self.file_tabs.iter().enumerate().filter_map(|(ix, file)| {
            let tab_ix = tabs.index(VisibleTab::File(ix))?;
            let e = file.editor.read(cx);
            let path = e.path.clone();
            let name: SharedString = path.rsplit('/').next().unwrap_or(&path).to_string().into();
            let on = sel.as_ref() == Some(&TabId::File(path.clone()));
            Some((
                VisibleTab::File(ix),
                pill(format!("file-tab-{path}"), on, theme)
                    .map(|d| self.tab_drag(d, TabId::File(path.clone()), name.clone(), cx))
                    .aria_label(path.clone())
                    .pl_2p5()
                    .pr_1()
                    .on_click(cx.listener(move |this, _, w, cx| this.select_file(ix, w, cx)))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, e: &MouseDownEvent, w, cx| {
                            this.select_file(ix, w, cx);
                            let entries = this.editor_menu(cx);
                            this.show_menu(e.position, entries, w, cx);
                        }),
                    )
                    .child(if ctrl_held && tab_ix < 9 {
                        kbd(&format!("ctrl-{}", tab_ix + 1)).into_any_element()
                    } else {
                        Icon::new(IconName::FileCode)
                            .with_size(px(14.))
                            .into_any_element()
                    })
                    .child(tab_label(name, on, theme).id("tab-label").test_support())
                    .when(e.dirty, |d| {
                        d.child(
                            div()
                                .flex_none()
                                .size(px(6.))
                                .rounded_full()
                                .bg(theme.foreground),
                        )
                    })
                    .child(tab_close(
                        crate::settings::close_button("close-file-tab", "Close file (cmd-w)")
                            .on_click(cx.listener(move |this, _, w, cx| {
                                cx.stop_propagation();
                                if let Some(editor) =
                                    this.file_tabs.get(ix).map(|t| t.editor.clone())
                                {
                                    this.close_editor(editor, w, cx);
                                }
                            })),
                        theme,
                    ))
                    .tooltip(move |w, cx| Tooltip::new(path.clone()).build(w, cx))
                    .map(|d| self.focus_mark(g, on, d, theme))
                    .test_support()
                    .into_any_element(),
            ))
        }));
        let pills: Vec<_> = tabs
            .0
            .iter()
            .filter_map(|(id, tab)| {
                let ix = pills.iter().position(|(t, _)| t == tab)?;
                Some(self.reorder_item(
                    pills.remove(ix).1,
                    reorder::Item::Tab(ws.name.clone(), id.clone()),
                ))
            })
            .collect();
        let (layout, scroll) = (self.tab_layout[g].clone(), self.tab_scroll[g].clone());
        let active = self.active_tab_index(g, cx).unwrap_or(0);
        let strip = h_flex()
            .on_children_prepainted(move |children, window, cx| {
                let next = (scroll.bounds().size.width, children.len());
                if layout.replace(next) != next {
                    // A resize or removed tab can hide the selection without changing it.
                    let scroll = scroll.clone();
                    window.defer(cx, move |window, _| {
                        scroll.scroll_to_item(active);
                        window.refresh();
                    });
                }
            })
            .id(match g {
                0 => SharedString::from("chat-tabs"),
                _ => format!("chat-tabs-{g}").into(),
            })
            .role(Role::TabList)
            .aria_label(match (split, g) {
                (false, _) => "Workspace tabs",
                (true, 0) => "Left group tabs",
                (true, _) => "Right group tabs",
            })
            .test_support()
            .min_w_0()
            .gap_1()
            .overflow_x_scroll()
            .track_scroll(&self.tab_scroll[g])
            .children(pills)
            .into_any_element();
        let left = self.tab_edge(g, Ordering::Less, ws, &tabs, cx);
        let right = self.tab_edge(g, Ordering::Greater, ws, &tabs, cx);
        let overflow = left.is_some() || right.is_some();
        let mut tabs = vec![];
        if overflow {
            tabs.push(
                div()
                    .w(px(24.))
                    .flex_none()
                    .children(left)
                    .into_any_element(),
            );
        }
        tabs.push(strip);
        if overflow {
            tabs.push(
                div()
                    .w(px(24.))
                    .flex_none()
                    .children(right)
                    .into_any_element(),
            );
        }
        tabs
    }

    /// Right-click on the `ix`th chat tab; closing it is offered where its close button is.
    pub(super) fn chat_tab_menu(&self, ix: usize, cx: &Context<Self>) -> Vec<Entry> {
        let Some(ws) = self.active_ws() else {
            return vec![];
        };
        let Some(meta) = ws.chats.get(ix) else {
            return vec![];
        };
        let mut entries = vec![menu::item(
            IconName::Plus,
            "New chat",
            on_shark(cx, |s, w, cx| s.new_chat(w, cx)),
        )];
        if !ws.closed.is_empty() {
            entries.push(menu::item(
                IconName::RotateCcw,
                "Reopen closed chat…",
                on_shark(cx, |s, w, cx| s.pick_closed_chat(w, cx)),
            ));
        }
        if let Some(id) = meta.session_id.clone() {
            entries.push(menu::item(
                IconName::Copy,
                "Copy session ID",
                move |w, cx| copy(id.clone(), w, cx),
            ));
        }
        let transcript = crate::agent::transcript_path(&meta.id);
        if transcript.exists() {
            entries.push(menu::item(
                IconName::FolderOpen,
                "Reveal transcript in Finder",
                move |_, cx| cx.reveal_path(&transcript),
            ));
        }
        if ws.chats.len() > 1 {
            entries.extend([
                Entry::Sep,
                menu::item(
                    IconName::X,
                    "Close chat",
                    on_shark(cx, |s, w, cx| s.close_chat(w, cx)),
                ),
            ]);
        }
        entries
    }

    /// Holds `caffeinate -i` while any agent works and the setting is on: no idle sleep, though
    /// the display may sleep and closing the lid still sleeps. `-w` ends it with us, even on a crash.
    pub(crate) fn update_awake(&mut self, cx: &App) {
        let want = self.state.stay_awake && self.chats.values().any(|c| c.read(cx).working());
        match (want, &mut self.awake) {
            (true, None) => {
                let pid = std::process::id().to_string();
                self.awake = std::process::Command::new("caffeinate")
                    .args(["-i", "-w", &pid])
                    .spawn()
                    .ok();
            }
            (false, Some(c)) => {
                let _ = c.kill();
                let _ = c.wait();
                self.awake = None;
            }
            _ => {}
        }
    }
}
