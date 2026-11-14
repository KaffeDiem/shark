//! Agents in chats: the composer's agent menu, and handing a chat to another agent in a new tab
//! with its transcript attached.
use super::menus::{on_shark, on_task};
use super::*;
use crate::chat::HandoffTarget;
use crate::menu::Entry;

/// Writes chat `id`'s transcript (and plan, if any) to the task's attachments as
/// `<title> transcript.md`; returns its path in the task. Reads from disk, so off the UI thread.
pub(super) fn write_transcript(dir: &Path, id: &str, title: &str) -> Result<String, String> {
    let plan = crate::plan::handoff_text(id);
    let path = crate::transcript::transcript_path(id);
    let md = crate::transcript::markdown(&path, title, plan.as_deref())
        .ok_or("this chat's transcript couldn't be read")?;
    let file = crate::complete::file_name(title);
    crate::complete::attach_bytes(dir, &format!("{file} transcript.md"), md.as_bytes())
}

impl Shark {
    /// The next agent of the chat's agent, for the bar after a finished turn.
    pub(super) fn handoff_target(&self, meta: &ChatMeta) -> Option<HandoffTarget> {
        let current = self.agent(meta.agent.as_deref()?)?;
        let next = self.agent(current.next.as_deref()?)?;
        (next.slug != current.slug).then(|| HandoffTarget {
            slug: next.slug.clone(),
            name: next.name.clone(),
            icon: next.icon.clone(),
        })
    }

    /// The agent button's menu. Before the first message it picks the chat's agent; after, the
    /// other agents take the chat over in a new tab.
    pub(super) fn chat_agent_menu(
        &mut self,
        chat: Entity<Chat>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let c = chat.read(cx);
        let (task, id, current, open) = (
            c.ws.name.clone(),
            c.meta.id.clone(),
            c.meta.agent.clone(),
            c.agent_open(),
        );
        let b = c.agent_at.get();
        let mut entries = vec![];
        if open {
            for a in &self.state.agents {
                let (slug, weak) = (a.slug.clone(), chat.downgrade());
                let pick = on_shark(cx, move |s, w, cx| {
                    s.set_chat_agent(&weak, Some(slug.clone()), w, cx)
                });
                let on = current.as_deref() == Some(a.slug.as_str());
                entries.push(
                    menu::item(None, a.name.clone(), pick)
                        .agent_icon(a.icon.clone())
                        .checked(on),
                );
            }
            let weak = chat.downgrade();
            entries.push(
                menu::item(
                    IconName::AtSign,
                    "No agent",
                    on_shark(cx, move |s, w, cx| s.set_chat_agent(&weak, None, w, cx)),
                )
                .checked(current.is_none()),
            );
        } else {
            let next = (current.as_deref())
                .and_then(|s| self.agent(s))
                .and_then(|a| a.next.clone());
            let mut agents: Vec<_> = (self.state.agents.iter())
                .filter(|a| current.as_deref() != Some(a.slug.as_str()))
                .collect();
            agents.sort_by_key(|a| next.as_deref() != Some(a.slug.as_str()));
            if !agents.is_empty() {
                entries.push(Entry::Heading(
                    "Hand off to".into(),
                    "Opens a new chat with this transcript".into(),
                ));
            }
            for a in agents {
                let (to, from) = (a.slug.clone(), id.clone());
                let go = on_task(cx, &task, move |s, n, w, cx| {
                    s.hand_off(&n, &to, Some(from.clone()), w, cx)
                });
                let hint = if next.as_deref() == Some(a.slug.as_str()) {
                    "next"
                } else {
                    ""
                };
                entries.push(
                    menu::item(None, a.name.clone(), go)
                        .agent_icon(a.icon.clone())
                        .hint(hint),
                );
            }
        }
        if !entries.is_empty() {
            entries.push(Entry::Sep);
        }
        if let Some(agent) = current.as_deref().and_then(|s| self.agent(s)) {
            let slug = agent.slug.clone();
            entries.push(menu::item(
                IconName::Settings,
                format!("Edit {}…", agent.name),
                on_shark(cx, move |s, w, cx| s.open_agent(&slug, w, cx)),
            ));
        }
        // A chat that hasn't started takes the agent it asked for.
        let weak = chat.downgrade();
        let then: Option<super::agent_settings::AfterCreate> = open.then(|| {
            Rc::new(
                move |s: &mut Shark, slug: &str, w: &mut Window, cx: &mut Context<Shark>| {
                    s.set_chat_agent(&weak, Some(slug.into()), w, cx)
                },
            ) as super::agent_settings::AfterCreate
        });
        entries.push(menu::item(
            IconName::Plus,
            "New agent…",
            on_shark(cx, move |s, w, cx| {
                s.agent_dialog(None, then.clone(), w, cx)
            }),
        ));
        let at = point(b.left(), b.top() - px(4.));
        self.show_menu_at(entries, at, Anchor::BottomLeft, window, cx);
    }

    /// cmd-k Choose agent…: the open chat's agent button menu.
    pub(super) fn pick_chat_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active_chat().cloned() {
            Some(chat) => self.chat_agent_menu(chat, window, cx),
            None => notify_err(window, cx, "Open a chat first"),
        }
    }

    /// Puts agent `slug` (or none) on a chat that hasn't started, with its model and thinking.
    pub(super) fn set_chat_agent(
        &mut self,
        chat: &WeakEntity<Chat>,
        slug: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(chat) = chat.upgrade() else { return };
        let task = chat.read(cx).ws.name.clone();
        let Some(ws) = self.task(&task) else { return };
        if slug.as_deref().is_some_and(|s| self.agent(s).is_none()) {
            return notify_err(window, cx, "The selected agent is no longer available");
        }
        let defaults = slug
            .as_deref()
            .map(|s| self.state.defaults_for(ws, Some(s)));
        if !chat.update(cx, |c, cx| c.set_agent(slug, defaults, cx)) {
            return;
        }
        let next = self.handoff_target(&chat.read(cx).meta);
        chat.update(cx, |c, cx| {
            c.next_agent = next;
            cx.notify();
        });
        self.record_interaction(&task, cx);
        chat.update(cx, |c, cx| c.focus_composer(window, cx));
    }

    /// Hand off: a new chat for `to`, as cmd-t opens one, with `from_chat`'s transcript attached
    /// in its composer, like a pasted image. The task, its repos and other chats stay as they are.
    pub(super) fn hand_off(
        &mut self,
        task: &str,
        to: &str,
        from_chat: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.task(task).filter(|_| !self.archiving.contains(task)) else {
            return notify_err(window, cx, "The selected task is no longer available");
        };
        if self.agent(to).is_none() {
            return notify_err(window, cx, "The selected agent is no longer available");
        }
        let source = (from_chat.as_deref())
            .and_then(|id| ws.chats.iter().chain(&ws.closed).find(|c| c.id == id))
            .or_else(|| ws.chats.get(ws.active_chat))
            .cloned();
        let Some(source) = source else {
            return notify_err(window, cx, "The selected chat is no longer available");
        };
        // The transcript is read from disk: what failed to save would be missing from it, and so
        // would a reply still arriving.
        if self.chats.get(&source.id).is_some_and(|c| c.read(cx).busy) {
            return notify_err(
                window,
                cx,
                "Wait for this chat's turn to finish, then hand it off",
            );
        }
        if let Some(chat) = self.chats.get(&source.id) {
            chat.update(cx, |c, cx| c.retry_history(cx));
            let plan = chat.read(cx).plan.clone();
            let error = plan.update(cx, |p, cx| {
                p.retry_save(cx);
                p.storage_error().map(str::to_owned)
            });
            if let Some(error) = error {
                return notify_err(window, cx, error);
            }
            if chat.read(cx).history_save_error.is_some() {
                return notify_err(
                    window,
                    cx,
                    "Couldn't save this chat's history. Retry the save before handing it off.",
                );
            }
        }
        let title = match source.title.trim() {
            "" => ws.display_title().to_string(),
            t => t.to_string(),
        };
        let (dir, task, to, fast) = (ws.dir(), task.to_string(), to.to_string(), source.fast);
        let instance = ws.instance_id.clone();
        let written = cx.background_executor().spawn(async move {
            write_transcript(&dir, &source.id, &title).map(|path| {
                // A second handoff of the same chat gets `… transcript (1).md`.
                let name = std::path::Path::new(&path).file_name().unwrap_or_default();
                let label = name.to_string_lossy().into_owned();
                (path, label)
            })
        });
        cx.spawn_in(window, async move |this, cx| {
            let written = written.await;
            this.update_in(cx, |s, window, cx| {
                let (path, label) = match written {
                    Ok(w) => w,
                    Err(e) => return notify_err(window, cx, format!("Couldn't hand off: {e}")),
                };
                // A task deleted while the transcript was written may have a successor by name.
                if s.task(&task).is_none_or(|w| w.instance_id != instance) {
                    return notify_err(window, cx, "The selected task is no longer available");
                }
                let name = task.clone();
                // The chat is added once the task shows: cancelling its unsaved-file prompt
                // leaves the task as it was.
                s.open_task_then(&name, None, window, cx, move |s, window, cx| {
                    if s.agent(&to).is_none() {
                        return notify_err(window, cx, "The selected agent is no longer available");
                    }
                    let Some(ws) = s.task_mut(&task) else { return };
                    let id = ws.next_chat_id();
                    let meta = ChatMeta {
                        fast,
                        ..s.state
                            .new_chat(s.task(&task).unwrap(), id.clone(), Some(to))
                    };
                    let at = s.task_mut(&task).unwrap().push_chat(meta);
                    let _ = s.state.save();
                    if let Err(e) = s.relink_agents(&task, cx) {
                        notify_err(window, cx, e);
                    }
                    s.select_chat(at, window, cx);
                    if let Some(chat) = s.chats.get(&id).cloned() {
                        chat.update(cx, |c, cx| {
                            c.attach_mention(&path, label, window, cx);
                            c.focus_composer(window, cx);
                        });
                    }
                });
            })
            .ok();
        })
        .detach();
    }

    /// Hand off to…: the agents to take the chat over, next first.
    pub(super) fn hand_off_picker(
        &mut self,
        task: &str,
        from_chat: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.task(task) else {
            return;
        };
        let source = (from_chat.as_deref())
            .and_then(|id| ws.chats.iter().chain(&ws.closed).find(|c| c.id == id))
            .or_else(|| ws.chats.get(ws.active_chat));
        let current = source.and_then(|c| c.agent.clone());
        let next = (current.as_deref())
            .and_then(|a| self.agent(a))
            .and_then(|a| a.next.clone());
        let mut agents: Vec<_> = (self.state.agents.iter())
            .filter(|a| current.as_deref() != Some(a.slug.as_str()))
            .cloned()
            .collect();
        agents.sort_by_key(|a| next.as_deref() != Some(a.slug.as_str()));
        if agents.is_empty() {
            return notify_err(window, cx, "Create another agent first");
        }
        let items = agents.iter().map(super::agent_icon_view::command).collect();
        let task = task.to_string();
        open_menu(
            cx.entity().downgrade(),
            "Hand off to…",
            items,
            0,
            move |s, i, w, cx| s.hand_off(&task, &agents[i].slug, from_chat.clone(), w, cx),
            window,
            cx,
        );
    }
}
