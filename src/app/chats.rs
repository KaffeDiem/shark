//! Chat tabs of the open workspace, and the agent status in the header.
use super::*;

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
        let chat = cx.new(|cx| {
            let mut c = Chat::new(ws.clone(), meta.clone(), window, cx);
            c.archiving = archiving;
            c
        });
        let (id, name) = (meta.id.clone(), ws.name.clone());
        // Busy also flips without a ChatEvent (background tasks, turns the CLI starts itself).
        self._subs.push(cx.observe(&chat, |this, _, cx| {
            this.update_awake(cx);
            cx.notify()
        }));
        self._subs.push(
            cx.subscribe(&chat, move |this, chat, ev: &ChatEvent, cx| match ev {
                ChatEvent::Meta => this.save_meta(&chat, cx),
                ChatEvent::TurnDone => {
                    if this.active.as_ref() == Some(&name) {
                        this.refresh_diff(cx);
                    }
                    if this.active_chat() != Some(&chat) {
                        this.unread.insert(id.clone());
                    }
                    this.turn_done(&name, cx);
                    cx.notify();
                }
            }),
        );
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
        if let Some(sync) = self.syncs.get(&ws.name) {
            sync.update(cx, |g, cx| g.set_chat(chat.clone(), cx));
        }
        if self.tab == Tab::Diff {
            self.tab = Tab::Chat;
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

    /// cmd-t: a new tab right of the current one, on the current chat's model.
    pub(super) fn new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let model = self
            .active_chat()
            .and_then(|c| c.read(cx).meta.model.clone());
        let Some(ws) = self.active_ws_mut() else {
            return notify_err(window, cx, "Open a workspace first");
        };
        let meta = ChatMeta {
            id: ws.next_chat_id(),
            model,
            ..Default::default()
        };
        let at = ws.active_chat + 1;
        ws.chats.insert(at, meta);
        self.state.save();
        self.select_chat(at, window, cx);
    }

    /// cmd-w: closes the shown chat, asking first if it's running. The last tab gives way to a
    /// fresh one, so a workspace always has a chat.
    pub(super) fn close_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        let Some(ws) = self.active_ws_mut() else {
            return;
        };
        let ix = ws.active_chat;
        let meta = ws.chats.remove(ix);
        if ws.chats.is_empty() {
            let id = ws.next_chat_id();
            ws.chats.push(ChatMeta {
                id,
                model: meta.model.clone(),
                ..Default::default()
            });
        }
        ws.active_chat = ix.min(ws.chats.len() - 1);
        let next = ws.active_chat;
        let (name, untouched) = (ws.name.clone(), meta.session_id.is_none());
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
        if let Some(chat) = self.active_chat() {
            chat.update(cx, |c, cx| c.stop(cx));
        }
    }

    pub(super) fn pick_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = self.active_chat().cloned() else {
            return notify_err(window, cx, "Open a workspace first");
        };
        let current = chat.read(cx).model().to_string();
        let items = MODELS
            .iter()
            .map(|m| {
                CommandItem::new()
                    .label(*m)
                    .icon(IconName::Sparkles)
                    .checked(*m == current)
            })
            .collect();
        let selected = MODELS.iter().position(|m| *m == current).unwrap_or(0);
        open_menu(
            cx.entity().downgrade(),
            items,
            selected,
            move |_, row, _, cx| chat.update(cx, |c, cx| c.set_model(MODELS[row], cx)),
            window,
            cx,
        );
    }

    /// The no-menu fast path, so confirm the switch in a toast.
    pub(super) fn set_model(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(chat), Some(m)) = (self.active_chat().cloned(), MODELS.get(ix)) else {
            return notify_err(window, cx, "Open a workspace first");
        };
        chat.update(cx, |c, cx| c.set_model(m, cx));
        window.push_notification(format!("Model: {m} (from the next message)"), cx);
    }

    /// One pill per chat, like the Diff and file tabs; a single chat reads "Chat" as before.
    pub(super) fn render_chat_tabs(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(ws) = self.active_ws() else {
            return vec![];
        };
        let theme = cx.theme();
        let many = ws.chats.len() > 1;
        let mut tabs: Vec<AnyElement> = (ws.chats.iter().enumerate())
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
                        // A finished turn you haven't looked at yet.
                        .when(unread, |d| {
                            d.child(
                                div()
                                    .absolute()
                                    .top(px(-2.))
                                    .right(px(-2.))
                                    .size(px(6.))
                                    .rounded_full()
                                    .bg(theme.primary),
                            )
                        })
                        .into_any_element(),
                };
                pill(format!("chat-tab-{}", meta.id), on, theme)
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
                            Button::new("close-chat")
                                .ghost()
                                .xsmall()
                                .icon(IconName::X)
                                .tooltip("Close chat (cmd-w)")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.close_chat(window, cx)
                                })),
                        )
                    })
                    .into_any_element()
            })
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

    /// The header's agent status, naming the call in flight; hovering shows the turn's details
    /// and what runs in the background.
    pub(super) fn render_status(&self, chat: &Entity<Chat>, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let c = chat.read(cx);
        let running = match c.running_tool() {
            Some(t) => format!("Running · {t}"),
            None => "Running".into(),
        };
        let text = match (c.busy, c.background.len()) {
            (true, 0) => running,
            (true, n) => format!("{running} · {n} in background"),
            (false, 0) => "Idle".into(),
            (false, n) => format!("{n} in background"),
        };
        let (busy, started, model) = (c.busy, c.started, c.model().to_string());
        let (queued, steered, tasks) = (c.queued(), c.steered, c.background.clone());
        HoverCard::new("status-card")
            .anchor(Anchor::TopRight)
            .trigger(
                h_flex()
                    .id("status")
                    .pl_3()
                    .gap_1p5()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(div().size(px(6.)).rounded_full().bg(if c.working() {
                        theme.primary
                    } else {
                        faint(theme)
                    }))
                    .child(div().max_w(px(360.)).truncate().child(text)),
            )
            .content(move |_, _, cx| {
                let theme = cx.theme();
                let line = |t: String| div().child(t);
                v_flex()
                    .gap_1()
                    .text_xs()
                    .max_w(px(420.))
                    .when_some(started.filter(|_| busy), |d, t| {
                        d.child(line(format!(
                            "Turn running for {}",
                            crate::chat::duration(t.elapsed().as_secs())
                        )))
                    })
                    .child(line(format!("Model {model}")))
                    .when(queued > 0, |d| d.child(line(format!("{queued} queued"))))
                    .when(steered, |d| {
                        d.child(line("Message sent mid-turn, not read yet".into()))
                    })
                    .when(!tasks.is_empty(), |d| {
                        d.child(
                            div()
                                .pt_1()
                                .text_color(theme.muted_foreground)
                                .child(format!("{} in background", tasks.len())),
                        )
                        .children(tasks.iter().map(|t| {
                            h_flex()
                                .gap_1p5()
                                .child(Icon::new(IconName::Bot).with_size(px(12.)))
                                .child(t.clone())
                        }))
                    })
            })
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
