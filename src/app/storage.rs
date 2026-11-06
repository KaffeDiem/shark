use super::*;

impl Shark {
    pub(super) fn set_delete_remote(&mut self, on: bool, cx: &mut Context<Self>) {
        self.state.delete_remote_branches = on;
        let _ = self.state.save();
        for sync in self.syncs.values() {
            sync.update(cx, |g, cx| {
                g.delete_remote = on;
                cx.notify();
            });
        }
        cx.notify();
    }

    /// Data that failed to save, as user-facing lines; `only` limits it to one workspace's.
    pub(super) fn unsaved_data(&self, only: Option<&str>, cx: &App) -> Vec<String> {
        let mut errors = Vec::new();
        let keep = |ws: &str| only.is_none_or(|o| o == ws);
        if let Some(e) = self
            .state
            .save_error
            .borrow()
            .as_ref()
            .filter(|_| only.is_none())
        {
            errors.push(format!("Workspace state and settings: {e}"));
        }
        for (ws, e) in self.differ.read(cx).storage_errors() {
            if keep(&ws) {
                errors.push(format!("Comments in {ws}: {e}"));
            }
        }
        for chat in self.chats.values() {
            let chat = chat.read(cx);
            let mine = only.is_none_or(|o| {
                (self.ws(o)).is_some_and(|w| {
                    w.chats
                        .iter()
                        .chain(&w.closed)
                        .any(|m| m.id == chat.meta.id)
                })
            });
            if let (true, Some(e)) = (mine, &chat.history_save_error) {
                errors.push(format!("Completed answers in {}: {e}", chat.meta.id));
            }
        }
        for (ws, git) in &self.git_features {
            if let (true, Some(e)) = (keep(ws), git.read(cx).storage_error()) {
                errors.push(format!("Git drafts in {ws}: {e}"));
            }
        }
        errors.sort();
        errors
    }

    pub(super) fn retry_saves(&mut self, cx: &mut Context<Self>) {
        self.flush_interaction();
        if self.state.save_error.borrow().is_some() {
            let _ = self.state.save();
        }
        self.differ.update(cx, |d, _| d.retry_saves());
        for chat in self.chats.values() {
            chat.update(cx, |c, cx| c.retry_history(cx));
        }
        for git in self.git_features.values() {
            git.update(cx, |g, cx| g.retry_save(cx));
        }
        cx.notify();
    }

    pub(super) fn storage_guard(
        &mut self,
        quitting: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.creating.is_empty() || !self.adding_repos.is_empty() || !self.archiving.is_empty()
        {
            window.push_notification("Wait for workspace changes to finish before closing", cx);
            return false;
        }
        if self.git_features.values().any(|g| g.read(cx).busy()) {
            window.push_notification("Wait for the Git operation to finish before closing", cx);
            return false;
        }
        self.retry_saves(cx);
        if self.unsaved_data(None, cx).is_empty() {
            if self
                .git_features
                .values()
                .any(|g| g.read(cx).saving_drafts())
            {
                window.push_notification("Wait for Git drafts to finish saving before closing", cx);
                return false;
            }
            return true;
        }
        let owner = cx.entity().downgrade();
        window.open_dialog(cx, move |d, _, cx| {
            let Some(owner) = owner.upgrade() else {
                return d;
            };
            owner.update(cx, |s, cx| {
                crate::ui::dialog(d)
                    .title("Unsaved data")
                    .child(div().child(s.unsaved_data(None, cx).join("\n")))
                    .footer(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("save-retry-close")
                                    .label("Retry")
                                    .primary()
                                    .on_click(cx.listener(move |s, _, w, cx| {
                                        w.close_dialog(cx);
                                        if quitting {
                                            s.quit(w, cx);
                                        } else if s.close_window(w, cx) {
                                            w.remove_window();
                                        }
                                    })),
                            )
                            .child(
                                Button::new("save-cancel-close")
                                    .label("Cancel")
                                    .on_click(|_, w, cx| w.close_dialog(cx)),
                            )
                            .child(
                                Button::new("save-abandon-close")
                                    .label("Quit without saving")
                                    .danger()
                                    .on_click(cx.listener(move |s, _, w, cx| {
                                        w.close_dialog(cx);
                                        s.confirm_discard(true, true, w, cx, move |_, w, cx| {
                                            if quitting {
                                                cx.quit();
                                            } else {
                                                w.remove_window();
                                            }
                                        });
                                    })),
                            ),
                    )
            })
        });
        false
    }
}
