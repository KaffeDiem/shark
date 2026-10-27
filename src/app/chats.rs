//! Chat tabs of the open workspace, and the agent status in the header.
use super::*;

/// Resolved entries in the persisted strip; unavailable files have no visible tab.
struct VisibleTabs(Vec<(TabId, VisibleTab)>);

#[derive(Clone, Copy, Debug, PartialEq)]
enum VisibleTab {
    Chat(usize),
    Diff,
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

#[derive(Clone)]
struct DraggedTab {
    workspace: String,
    id: TabId,
    label: SharedString,
}

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        sidebar::drag_chip(self.label.clone(), cx)
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
    fn visible_tabs(&self, ws: &Workspace, cx: &App) -> VisibleTabs {
        VisibleTabs(
            ws.tab_order
                .iter()
                .filter_map(|id| {
                    let tab = match id {
                        TabId::Chat(id) => {
                            VisibleTab::Chat(ws.chats.iter().position(|c| &c.id == id)?)
                        }
                        TabId::Diff if !ws.diff_closed => VisibleTab::Diff,
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

    pub(super) fn active_tab_index(&self, cx: &App) -> Option<usize> {
        let ws = self.active_ws()?;
        let tab = match self.tab {
            Tab::Chat => VisibleTab::Chat(ws.active_chat),
            Tab::Diff => VisibleTab::Diff,
            Tab::File => VisibleTab::File(
                self.file_tabs
                    .iter()
                    .position(|t| Some(&t.editor) == self.editor.as_ref())?,
            ),
        };
        self.visible_tabs(ws, cx).index(tab)
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
            move |this, chat, ev: &ChatEvent, window, cx| match ev {
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
                    if this.active_chat() != Some(chat) {
                        this.unread.insert(id.clone());
                    }
                    this.refresh_home(Some(&name), cx);
                    this.turn_done(&name, cx);
                    this.review_turn_done(&name, chat, cx);
                    cx.notify();
                }
                // A question off screen marks its tab; the workspace needs you until it's answered.
                ChatEvent::Asking => {
                    if this.active_chat() != Some(chat) && chat.read(cx).asking() {
                        this.unread.insert(id.clone());
                    }
                    this.refresh_home(Some(&name), cx);
                    this.refresh_attention(cx);
                }
                ChatEvent::ShowChanges => this.show(Tab::Diff, window, cx),
                ChatEvent::ShowComment(id) => {
                    this.show(Tab::Diff, window, cx);
                    this.jump_to_comment(*id, window, cx);
                }
                ChatEvent::ReviewRemoved(ids) => this.review_delivered(&name, &id, ids, None, cx),
                ChatEvent::ReviewSent { ids, turn } => {
                    this.review_delivered(&name, &id, ids, Some(*turn), cx)
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
        let tabs = self.visible_tabs(ws, cx);
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

    fn tab_drag(
        &self,
        pill: Stateful<Div>,
        id: TabId,
        label: SharedString,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let workspace = self.active.clone().unwrap();
        let (accent, foreground) = (cx.theme().accent, cx.theme().accent_foreground);
        // A press must keep content focus until a click actually selects the tab.
        pill.on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_drag(
                DraggedTab {
                    workspace: workspace.clone(),
                    id: id.clone(),
                    label,
                },
                |d, _, _, cx| cx.new(|_| d.clone()),
            )
            .drag_over::<DraggedTab>({
                let (workspace, id) = (workspace.clone(), id.clone());
                move |style, drag, _, _| {
                    if drag.workspace == workspace && drag.id != id {
                        style.bg(accent).text_color(foreground)
                    } else {
                        style
                    }
                }
            })
            .on_drop(cx.listener(move |this, drag: &DraggedTab, _, cx| {
                this.move_tab(&drag.workspace, &drag.id, &id, cx)
            }))
    }

    pub(super) fn reveal_tab(&self, cx: &App) {
        if let Some(active) = self.active_tab_index(cx) {
            self.tab_scroll.scroll_to_item(active);
        }
    }

    pub(super) fn cycle_chat(&mut self, d: isize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ws) = self.active_ws() {
            let n = ws.chats.len() as isize;
            let ix = (ws.active_chat as isize + d).rem_euclid(n) as usize;
            self.select_chat(ix, window, cx);
        }
    }

    /// Ctrl-tab follows the visible strip: the saved mixed tab order.
    pub(super) fn cycle_tab(&mut self, d: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.home_shown() {
            return self.cycle_chat(d, window, cx);
        }
        let Some(ws) = self.active_ws() else { return };
        let Some(active) = self.active_tab_index(cx) else {
            return;
        };
        let next =
            (active as isize + d).rem_euclid(self.visible_tabs(ws, cx).len() as isize) as usize;
        self.select_tab(next, window, cx);
    }

    pub(super) fn select_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.active_ws() else { return };
        match self.visible_tabs(ws, cx).get(ix) {
            Some(VisibleTab::Chat(ix)) => self.select_chat(ix, window, cx),
            Some(VisibleTab::Diff) => self.show(Tab::Diff, window, cx),
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
            Tab::Diff => self.close_diff(window, cx),
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
        let meta = ws.chats.remove(ix);
        if ws.chats.is_empty() {
            ws.chats.push(ChatMeta {
                id: fresh_id,
                ..fresh
            });
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
        self.review.edit(&name, |l| {
            review::unqueue(l, Some(&meta.id), &unread);
        });
        if let Some(chat) = self.chats.remove(&meta.id)
            && let Some(sync) = self.syncs.get(&name)
        {
            sync.update(cx, |g, cx| g.remove_chat(&chat, cx)); // the last ref; dropping stops claude
        }
        if self.active.as_deref() == Some(workspace) && selected == id {
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
            s.end_edit(cx);
            let ws = s.active_ws_mut().unwrap();
            ws.diff_closed = true;
            ws.close_tab(&TabId::Diff);
            let _ = s.state.save();
            if s.tab == Tab::Diff {
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

    /// Where tab `ix` sits against the tab strip as of the last layout: `Less` hidden (partly
    /// counts) past the left edge, `Greater` past the right, `Equal` in view.
    pub(super) fn tab_hidden(&self, ix: usize) -> Ordering {
        let Some(b) = self.tab_scroll.bounds_for_item(ix) else {
            return Ordering::Equal;
        };
        let (view, x) = (self.tab_scroll.bounds(), self.tab_scroll.offset().x);
        if b.left() + x < view.left() - px(1.) {
            Ordering::Less
        } else if b.right() + x > view.right() + px(1.) {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }

    /// An arrow at an edge of the tab strip while tabs hide past it, dotted when one of them
    /// finished a turn unseen; clicking brings that one (else the nearest hidden tab) into view.
    fn tab_edge(
        &self,
        edge: Ordering,
        ws: &Workspace,
        tabs: &VisibleTabs,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let mut hidden: Vec<usize> = (0..tabs.len())
            .filter(|&ix| self.tab_hidden(ix) == edge)
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
                            this.tab_scroll.scroll_to_item(to);
                            cx.on_next_frame(window, |_, _, cx| cx.notify());
                            cx.notify();
                        })),
                )
                .when(!unread.is_empty(), |d| d.child(dot(2., theme)))
                .into_any_element(),
        )
    }

    /// Chats, Diff and files share equal-width tabs that shrink to fit the header.
    pub(super) fn render_tabs(&self, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(ws) = self.active_ws() else {
            return vec![];
        };
        let theme = cx.theme();
        let tabs = self.visible_tabs(ws, cx);
        let m = window.modifiers();
        let ctrl_held = m.control && m.number_of_modifiers() == 1;
        let mut pills: Vec<(VisibleTab, AnyElement)> = (ws.chats.iter().enumerate())
            .map(|(ix, meta)| {
                let tab_ix = tabs.index(VisibleTab::Chat(ix)).unwrap();
                let chat = self.chats.get(&meta.id).map(|c| c.read(cx));
                let on = ix == ws.active_chat && self.tab == Tab::Chat;
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
                (
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
                        .test_support()
                        .into_any_element(),
                )
            })
            .collect();
        if let Some(ix) = tabs.index(VisibleTab::Diff) {
            pills.push((
                VisibleTab::Diff,
                pill("diff-tab", self.tab == Tab::Diff, theme)
                    .map(|d| self.tab_drag(d, TabId::Diff, "Diff".into(), cx))
                    .aria_label("Diff")
                    .pl_2p5()
                    .pr_1()
                    .on_click(cx.listener(|this, _, w, cx| this.show(Tab::Diff, w, cx)))
                    .child(if ctrl_held && ix < 9 {
                        kbd(&format!("ctrl-{}", ix + 1)).into_any_element()
                    } else {
                        Icon::new(IconName::GitCompare)
                            .with_size(px(14.))
                            .into_any_element()
                    })
                    .child(
                        tab_label("Diff".into(), self.tab == Tab::Diff, theme)
                            .id("tab-label")
                            .test_support(),
                    )
                    .child(tab_close(
                        crate::settings::close_button("close-diff", "Close Diff (cmd-w)").on_click(
                            cx.listener(|this, _, w, cx| {
                                cx.stop_propagation();
                                this.close_diff(w, cx);
                            }),
                        ),
                        theme,
                    ))
                    .tooltip(|w, cx| Tooltip::new("Diff (cmd-shift-d)").build(w, cx))
                    .test_support()
                    .into_any_element(),
            ));
        }
        pills.extend(self.file_tabs.iter().enumerate().map(|(ix, file)| {
            let e = file.editor.read(cx);
            let path = e.path.clone();
            let name: SharedString = path.rsplit('/').next().unwrap_or(&path).to_string().into();
            let on = self.tab == Tab::File && self.editor.as_ref() == Some(&file.editor);
            let tab_ix = tabs.index(VisibleTab::File(ix)).unwrap();
            (
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
                    .test_support()
                    .into_any_element(),
            )
        }));
        let pills: Vec<_> = tabs
            .0
            .iter()
            .filter_map(|(_, tab)| {
                let ix = pills.iter().position(|(t, _)| t == tab)?;
                Some(pills.remove(ix).1)
            })
            .collect();
        let (layout, scroll) = (self.tab_layout.clone(), self.tab_scroll.clone());
        let active = self.active_tab_index(cx).unwrap_or(0);
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
            .id("chat-tabs")
            .role(Role::TabList)
            .aria_label("Workspace tabs")
            .test_support()
            .min_w_0()
            .gap_1()
            .overflow_x_scroll()
            .track_scroll(&self.tab_scroll)
            .children(pills)
            .into_any_element();
        let left = self.tab_edge(Ordering::Less, ws, &tabs, cx);
        let right = self.tab_edge(Ordering::Greater, ws, &tabs, cx);
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
        tabs.push(
            Button::new("new-chat")
                .ghost()
                .xsmall()
                .icon(IconName::Plus)
                .w(px(24.))
                .flex_none()
                .tooltip("New chat (cmd-t)")
                .on_click(cx.listener(|this, _, window, cx| this.new_chat(window, cx)))
                .into_any_element(),
        );
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
