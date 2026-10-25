//! Chat tabs of the open workspace, and the agent status in the header.
use super::*;
use std::cmp::Ordering;

/// The accent dot on a tab or edge arrow: a finished turn you haven't looked at yet.
fn dot(inset: f32, theme: &Theme) -> Div {
    (div().absolute().top(px(inset)).right(px(inset)))
        .size(px(6.))
        .rounded_full()
        .bg(theme.primary)
}

impl Shark {
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
                ChatEvent::Meta => this.save_meta(&chat, cx),
                ChatEvent::Picked => {
                    // The level the chat runs at, so an old chat with none doesn't clear it.
                    let m = &chat.read(cx).meta;
                    let pick = (m.model.clone(), Some(crate::chat::effort_of(m).to_string()));
                    let st = &mut this.state;
                    if (&st.last_model, &st.last_effort) != (&pick.0, &pick.1) {
                        (st.last_model, st.last_effort) = pick;
                        st.save();
                    }
                }
                ChatEvent::TurnDone => {
                    if this.active.as_ref() == Some(&name) {
                        this.refresh_diff(cx);
                    }
                    if this.active_chat() != Some(&chat) {
                        this.unread.insert(id.clone());
                    }
                    this.refresh_home(Some(&name), cx);
                    this.turn_done(&name, cx);
                    this.review_turn_done(&name, chat, cx);
                    cx.notify();
                }
                ChatEvent::ShowChanges => this.show(Tab::Diff, window, cx),
                ChatEvent::ShowComment(id) => {
                    this.show(Tab::Diff, window, cx);
                    this.jump_to_comment(*id, window, cx);
                }
                ChatEvent::ReviewRemoved(ids) => this.review_delivered(&name, ids, None, cx),
                ChatEvent::ReviewSent { ids, turn } => {
                    this.review_delivered(&name, ids, Some(*turn), cx)
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
            self.state.save();
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
            self.state.save();
        }
        let ws = self.active_ws().cloned().unwrap();
        let chat = self.ensure_chat(&ws, &meta, window, cx);
        self.unread.remove(&meta.id);
        // Scrolled at the next layout; render again then so the edge arrows see where it landed.
        self.tab_scroll.scroll_to_item(ix);
        cx.on_next_frame(window, |_, _, cx| cx.notify());
        if let Some(sync) = self.syncs.get(&ws.name) {
            sync.update(cx, |g, cx| g.set_chat(chat.clone(), cx));
        }
        if self.tab == Tab::Diff {
            self.tab = Tab::Chat;
        }
        // cmd-t, cmd-shift-t, next/prev chat on Home: show the chat they acted on.
        if self.home.on {
            self.home.on = false;
            self.remember_open();
        }
        chat.update(cx, |c, cx| c.focus_composer(window, cx));
        cx.notify();
    }

    pub(super) fn cycle_chat(&mut self, d: isize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ws) = self.active_ws() {
            let n = ws.chats.len() as isize;
            let ix = (ws.active_chat as isize + d).rem_euclid(n) as usize;
            self.select_chat(ix, window, cx);
        }
    }

    /// cmd-t: a new tab right of the current one, on the last picked model and thinking level
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
        let at = ws.active_chat + 1;
        ws.chats.insert(at, meta);
        self.state.save();
        self.select_chat(at, window, cx);
    }

    /// cmd-w: closes the shown chat, asking first if it's running. The last tab gives way to a
    /// fresh one, so a workspace always has a chat.
    pub(super) fn close_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The chat isn't on screen.
        if self.home_shown() {
            return;
        }
        let Some(chat) = self.active_chat().cloned() else {
            return;
        };
        if !chat.read(cx).working() {
            return self.close_chat_now(window, cx);
        }
        confirm(
            format!("Stop and close “{}”?", chat.read(cx).title()),
            "The agent is still running in this chat. Closing stops it; cmd-shift-t reopens the chat.",
            "Stop and close",
            Self::close_chat_now,
            window,
            cx,
        );
    }

    fn close_chat_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let fresh = self.state.new_chat(String::new());
        let Some(ws) = self.active_ws_mut() else {
            return;
        };
        let ix = ws.active_chat;
        let meta = ws.chats.remove(ix);
        if ws.chats.is_empty() {
            let id = ws.next_chat_id();
            ws.chats.push(ChatMeta { id, ..fresh });
        }
        ws.active_chat = ix.min(ws.chats.len() - 1);
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
        self.state.save();
        for m in evicted.iter().chain(untouched.then_some(&meta)) {
            let _ = std::fs::remove_file(crate::agent::transcript_path(&m.id));
            crate::plan::delete(&m.id);
        }
        self.unread.remove(&meta.id);
        if let Some(chat) = self.chats.remove(&meta.id)
            && let Some(sync) = self.syncs.get(&name)
        {
            sync.update(cx, |g, cx| g.remove_chat(&chat, cx)); // the last ref; dropping stops claude
        }
        self.select_chat(next, window, cx);
    }

    /// cmd-shift-t: brings back the most recently closed chat, or a picked one.
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
        let at = ws.active_chat + 1;
        ws.chats.insert(at, meta);
        self.state.save();
        self.select_chat(at, window, cx);
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
        if self.tab == Tab::Diff {
            self.tab = Tab::Chat;
        }
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
    pub(crate) fn running_chats(&self, ws: &str, cx: &App) -> usize {
        (self.started_chats(ws).iter())
            .filter(|c| c.read(cx).working())
            .count()
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
            return notify_err(window, cx, "No plan yet; the agent submits one in plan mode");
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
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let mut hidden: Vec<usize> = (0..ws.chats.len())
            .filter(|&ix| self.tab_hidden(ix) == edge)
            .collect();
        if edge == Ordering::Less {
            hidden.reverse(); // nearest first
        }
        let unread: Vec<usize> = (hidden.iter().copied())
            .filter(|&ix| self.unread.contains(&ws.chats[ix].id))
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

    /// One pill per chat, like the Diff and file tabs; a single chat reads "Chat" as before.
    /// Too many to fit, and the strip scrolls sideways between edge arrows.
    pub(super) fn render_chat_tabs(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(ws) = self.active_ws() else {
            return vec![];
        };
        let theme = cx.theme();
        let many = ws.chats.len() > 1;
        let pills: Vec<AnyElement> = (ws.chats.iter().enumerate())
            .map(|(ix, meta)| {
                let chat = self.chats.get(&meta.id).map(|c| c.read(cx));
                let on = ix == ws.active_chat && self.tab == Tab::Chat;
                let current = ix == ws.active_chat;
                let label: SharedString = match (many, chat) {
                    (false, _) => "Chat".into(),
                    (true, Some(c)) => c.title(),
                    (true, None) if meta.title.is_empty() => "New chat".into(),
                    (true, None) => meta.title.clone().into(),
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
                pill(format!("chat-tab-{}", meta.id), on, theme)
                    .flex_none()
                    .pl_2p5()
                    .when(!(many && current), |d| d.pr_2p5())
                    .when(many && current, |d| d.pr_1())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_chat(ix, window, cx);
                        this.show(Tab::Chat, window, cx);
                    }))
                    .child(icon)
                    .child(div().max_w(px(180.)).truncate().child(label))
                    .when(many && current, |d| {
                        d.child(
                            crate::settings::close_button("close-chat", "Close chat (cmd-w)")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.close_chat(window, cx)
                                })),
                        )
                    })
                    .into_any_element()
            })
            .collect();
        let strip = h_flex()
            .id("chat-tabs")
            .min_w_0()
            .gap_1()
            .overflow_x_scroll()
            .track_scroll(&self.tab_scroll)
            .children(pills)
            .into_any_element();
        let mut tabs: Vec<AnyElement> = (self.tab_edge(Ordering::Less, ws, cx).into_iter())
            .chain([strip])
            .chain(self.tab_edge(Ordering::Greater, ws, cx))
            .collect();
        tabs.push(
            Button::new("new-chat")
                .ghost()
                .xsmall()
                .icon(IconName::Plus)
                .tooltip("New chat (cmd-t)")
                .on_click(cx.listener(|this, _, window, cx| this.new_chat(window, cx)))
                .into_any_element(),
        );
        tabs
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
