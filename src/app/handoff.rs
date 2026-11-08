//! Explicit transitions between agents, preserving the task and its chats.
use super::*;
use crate::chat::HandoffTarget;
use crate::state::{AgentLink, Attachment, Handoff};

impl Shark {
    pub(super) fn handoff_target(&self, task: &Task, meta: &ChatMeta) -> Option<HandoffTarget> {
        let current = task.agent.as_ref()?;
        if meta.agent.as_deref() != Some(&current.slug) {
            return None;
        }
        let next = self.agent(self.agent(&current.slug)?.next.as_deref()?)?;
        (next.slug != current.slug).then(|| HandoffTarget {
            slug: next.slug.clone(),
            name: next.name.clone(),
            icon: next.icon.clone(),
        })
    }

    fn handoff_ready(&self, task: &str, to: &str) -> Result<(), &'static str> {
        let ws = self
            .task(task)
            .filter(|_| !self.archiving.contains(task))
            .ok_or("The selected task is no longer available")?;
        if self.agent(to).is_none() {
            return Err("The selected agent is no longer available");
        }
        if ws.agent.as_ref().is_some_and(|a| a.slug == to) {
            return Err("This task already belongs to that agent");
        }
        if !ws.attaching.is_empty() || self.adding_repos.contains(task) {
            return Err("Wait for the repository to finish attaching");
        }
        Ok(())
    }

    pub(super) fn hand_off(
        &mut self,
        task: &str,
        to: &str,
        from_chat: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(e) = self.handoff_ready(task, to) {
            return notify_err(window, cx, e);
        }
        let ws = self.task(task).unwrap();
        let source = from_chat
            .as_deref()
            .and_then(|id| ws.chats.iter().chain(&ws.closed).find(|c| c.id == id))
            .or_else(|| ws.chats.get(ws.active_chat))
            .cloned();
        let instance = ws.instance_id.clone();
        let old = ws.agent.clone();
        let from = source
            .as_ref()
            .and_then(|c| c.agent.as_deref())
            .map(|slug| {
                self.agent(slug)
                    .map_or_else(|| slug.to_string(), |a| a.name.clone())
            })
            .unwrap_or_else(|| "Previous chat".into());
        if let Some(chat) = source.as_ref().and_then(|meta| self.chats.get(&meta.id)) {
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
                    "Answer sent, but could not save it to history. Retry save before handing off this chat.",
                );
            }
        }
        let (task, to) = (task.to_string(), to.to_string());
        let payload = cx.background_executor().spawn(async move {
            source.and_then(|meta| {
                let text = crate::plan::handoff_text(&meta.id).unwrap_or_else(|| {
                    crate::transcript::last_reply(&crate::transcript::transcript_path(&meta.id))
                });
                (!text.trim().is_empty()).then_some(Handoff {
                    from,
                    title: meta.title,
                    text,
                })
            })
        });
        cx.spawn_in(window, async move |this, cx| {
            let payload = payload.await;
            this.update_in(cx, |s, window, cx| {
                let name = task.clone();
                s.open_task_then(&name, None, window, cx, move |s, window, cx| {
                    if let Err(e) = s.handoff_ready(&task, &to) {
                        return notify_err(window, cx, e);
                    }
                    let ws = s.task(&task).unwrap();
                    if ws.instance_id != instance
                        || ws.agent.as_ref().map(|a| &a.slug) != old.as_ref().map(|a| &a.slug)
                    {
                        return notify_err(
                            window,
                            cx,
                            "The task changed while preparing the handoff; try again",
                        );
                    }
                    let agent = s.agent(&to).unwrap().clone();
                    let original = ws.clone();
                    let mut updated = original.clone();
                    updated.agent = Some(AgentLink::from(&agent));
                    let id = updated.next_chat_id();
                    let meta = ChatMeta {
                        handoff: payload,
                        fast: s.active_chat().and_then(|c| c.read(cx).meta.fast),
                        ..s.state.new_chat(&updated, id)
                    };
                    let at = updated.push_chat(meta);
                    if let Err(e) = git::write_claude_md(&updated) {
                        let restore = git::write_claude_md(&original);
                        return notify_err(
                            window,
                            cx,
                            format!(
                                "Couldn't write agent instructions: {e}{}",
                                restore
                                    .err()
                                    .map(|e| format!("; couldn't restore instructions: {e}"))
                                    .unwrap_or_default()
                            ),
                        );
                    }
                    *s.task_mut(&task).unwrap() = updated;
                    if let Err(e) = s.state.save() {
                        *s.task_mut(&task).unwrap() = original;
                        let restore = git::write_claude_md(s.task(&task).unwrap());
                        return notify_err(
                            window,
                            cx,
                            format!(
                                "Couldn't save handoff: {e}{}",
                                restore
                                    .err()
                                    .map(|e| format!("; couldn't restore instructions: {e}"))
                                    .unwrap_or_default()
                            ),
                        );
                    }
                    if let Some(old) = old {
                        if let Err(e) = s.sync_agent(&old.slug, false, cx) {
                            notify_err(window, cx, e);
                        }
                    }
                    if let Err(e) = s.sync_agent(&to, true, cx) {
                        notify_err(window, cx, e);
                    }
                    s.collapsed_agents.remove(&to);
                    s.select_chat(at, window, cx);
                    if let Some(chat) = s.active_chat().cloned() {
                        chat.update(cx, |c, cx| c.focus_composer(window, cx));
                    }
                    let ws = s.task(&task).unwrap();
                    let missing = s
                        .state
                        .repos
                        .iter()
                        .filter(|r| {
                            agent.repos.contains(&r.name)
                                && !ws.repos.iter().any(|a| a.name == r.name)
                        })
                        .map(|r| Attachment::fresh(r, &ws.default_branch(), ""))
                        .collect::<Vec<_>>();
                    if !missing.is_empty() {
                        s.queue_attachment(task, missing, window, cx);
                    }
                    cx.notify();
                });
            })
            .ok();
        })
        .detach();
    }

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
        let next = ws
            .agent
            .as_ref()
            .and_then(|a| self.agent(&a.slug))
            .and_then(|a| a.next.as_deref());
        let mut agents: Vec<_> = self
            .state
            .agents
            .iter()
            .filter(|a| ws.agent.as_ref().is_none_or(|c| c.slug != a.slug))
            .cloned()
            .collect();
        agents.sort_by_key(|a| Some(a.slug.as_str()) != next);
        if agents.is_empty() {
            return notify_err(window, cx, "Create another agent first");
        }
        let items = agents
            .iter()
            .map(|a| super::agent_icon_view::command(a))
            .collect();
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
