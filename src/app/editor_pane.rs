//! The file editor next to the chat.
use super::*;

impl Shark {
    pub(super) fn open_file(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(f) = self.diff_files.get(ix) else {
            return;
        };
        let path = f.path().to_string();
        let open = (self.editor.as_ref()).is_some_and(|e| e.read(cx).path == path);
        if !open && (f.status() == DiffFileStatus::Deleted || f.is_binary()) {
            return notify_err(
                window,
                cx,
                format!("{path} is deleted or binary; it shows in the diff only"),
            );
        }
        self.open_path(path, window, cx)
    }

    /// Opens `repo/rel/path` of the open workspace in the editor next to the chat.
    pub(super) fn open_path(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.active_ws() else {
            return;
        };
        if self
            .editor
            .as_ref()
            .is_some_and(|e| e.read(cx).path == path)
        {
            return self.show(Tab::File, window, cx);
        }
        let abs = ws.dir().join(&path);
        if std::fs::metadata(&abs).is_ok_and(|m| m.len() > crate::editor::MAX_BYTES) {
            return notify_err(
                window,
                cx,
                format!("{path} is too large to edit; it shows in the diff only"),
            );
        }
        self.confirm_discard(window, cx, move |s, window, cx| {
            let vim = s.state.vim;
            let editor = cx.new(|cx| FileEditor::new(path, abs, vim, window, cx));
            s._editor_subs = vec![
                cx.subscribe_in(
                    &editor,
                    window,
                    |s, _, ev: &EditorEvent, window, cx| match ev {
                        EditorEvent::Saved => s.refresh_diff(cx),
                        EditorEvent::Leave => {
                            let pane = if s.show_tree { Pane::Tree } else { Pane::Chat };
                            s.focus_pane(pane, window, cx)
                        }
                    },
                ),
                cx.observe(&editor, |s, editor, cx| {
                    let e = editor.read(cx);
                    let header = (
                        e.dirty,
                        e.conflict,
                        e.vim.status(),
                        e.vim.pending().to_string(),
                    );
                    if header != s.editor_header {
                        s.editor_header = header;
                        cx.notify();
                    }
                }),
            ];
            s.editor = Some(editor);
            if !s.rail {
                s.rail = true;
                s.rail_auto = true;
            }
            s.show(Tab::File, window, cx);
        });
    }

    pub(super) fn close_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor.is_none() {
            return;
        }
        self.confirm_discard(window, cx, |s, window, cx| {
            s.drop_editor();
            if s.tab == Tab::File {
                s.show(Tab::Diff, window, cx);
            }
        });
    }

    pub(super) fn drop_editor(&mut self) {
        self.editor = None;
        self._editor_subs.clear();
        if self.rail_auto {
            self.rail = false;
            self.rail_auto = false;
        }
    }

    /// Runs `then` now, or after the user agrees to drop unsaved edits in the open file.
    /// Quit, asking first if the open file has unsaved edits.
    pub fn quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_discard(window, cx, |_, _, cx| cx.quit())
    }

    pub(super) fn confirm_discard(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Shark, &mut Window, &mut Context<Shark>) + 'static,
    ) {
        let Some(path) = self
            .editor
            .as_ref()
            .filter(|e| e.read(cx).dirty)
            .map(|e| e.read(cx).path.clone())
        else {
            return then(self, window, cx);
        };
        let then = RefCell::new(Some(then));
        confirm(
            format!("Discard changes to {path}?"),
            "Your edits are not saved. Cancel and press cmd-s to keep them.",
            "Discard",
            move |s, window, cx| {
                if let Some(f) = then.borrow_mut().take() {
                    if let Some(e) = &s.editor {
                        e.update(cx, |e, _| e.dirty = false);
                    }
                    f(s, window, cx)
                }
            },
            window,
            cx,
        );
    }

    pub(super) fn toggle_vim(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.vim = !self.state.vim;
        self.state.save();
        let on = self.state.vim;
        if let Some(e) = &self.editor {
            e.update(cx, |e, cx| e.set_vim(on, cx));
        }
        window.push_notification(format!("Vim keys {}", if on { "on" } else { "off" }), cx);
        cx.notify();
    }

    pub(super) fn render_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(editor) = self.editor.clone() else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let e = editor.read(cx);
        let path = e.path.clone();
        let ix = self.diff_files.iter().position(|f| *f.path() == path);
        let viewed = ix.map(|ix| self.viewed(ix));
        let (dir, name) = match path.rsplit_once('/') {
            Some((d, n)) => (format!("{d}/"), n.to_string()),
            None => (String::new(), path.clone()),
        };
        let vim = e.vim_on.then(|| {
            let p = e.vim.pending();
            match p.is_empty() {
                true => e.vim.status().to_string(),
                false => format!("{} {p}", e.vim.status()),
            }
        });
        let (dirty, conflict) = (e.dirty, e.conflict);
        // Leader keys (`space …`) work in Vim's Normal mode, where space types nothing.
        let normal =
            e.vim_on && e.vim.mode == crate::vim::Mode::Normal && e.vim.pending().is_empty();
        v_flex()
            .key_context(if normal {
                "FileEditor VimNormal"
            } else {
                "FileEditor"
            })
            .size_full()
            .border_l_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .h(px(36.))
                    .flex_none()
                    .px_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_sm()
                    .child(
                        Icon::new(IconName::FileCode)
                            .with_size(px(14.))
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme.muted_foreground)
                                    .child(dir),
                            )
                            .child(div().flex_none().child(name))
                            .when(dirty, |d| {
                                d.child(
                                    div()
                                        .id("dirty")
                                        .ml_2()
                                        .size(px(6.))
                                        .rounded_full()
                                        .bg(theme.foreground)
                                        .tooltip(|w, cx| {
                                            Tooltip::new("Unsaved (cmd-s)").build(w, cx)
                                        }),
                                )
                            }),
                    )
                    .child(
                        div()
                            .id("vim-pill")
                            .px_2()
                            .h(px(20.))
                            .flex()
                            .items_center()
                            .rounded_full()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .when(vim.is_some(), |d| d.bg(theme.secondary))
                            .hover(|d| d.text_color(theme.foreground))
                            .child(vim.unwrap_or_else(|| "VIM OFF".into()))
                            .tooltip(|w, cx| {
                                Tooltip::new("Toggle Vim keys (ctrl-cmd-v)").build(w, cx)
                            })
                            .on_click(cx.listener(|this, _, w, cx| this.toggle_vim(w, cx))),
                    )
                    .when_some(viewed, |d, v| {
                        let ix = ix.unwrap_or_default();
                        d.child(
                            Checkbox::new("editor-viewed")
                                .label("Viewed")
                                .small()
                                .checked(v == Viewed::Yes)
                                .on_click(cx.listener(move |this, _: &bool, _, cx| {
                                    this.toggle_viewed(ix, cx)
                                })),
                        )
                    })
                    .child(
                        Button::new("close-file")
                            .ghost()
                            .xsmall()
                            .icon(IconName::X)
                            .tooltip("Close file (cmd-w)")
                            .on_click(cx.listener(|this, _, w, cx| this.close_file(w, cx))),
                    ),
            )
            .when(conflict, |d| {
                let (e1, e2) = (editor.clone(), editor.clone());
                d.child(
                    h_flex()
                        .h(px(36.))
                        .flex_none()
                        .px_3()
                        .gap_2()
                        .border_b_1()
                        .border_color(theme.border)
                        .text_sm()
                        .child(
                            Icon::new(IconName::TriangleAlert)
                                .with_size(px(14.))
                                .text_color(theme.warning_foreground),
                        )
                        .child(
                            div()
                                .flex_1()
                                .child("Changed on disk since you started editing"),
                        )
                        .child(
                            Button::new("reload")
                                .small()
                                .outline()
                                .label("Reload")
                                .on_click(move |_, w, cx| e1.update(cx, |e, cx| e.discard(w, cx))),
                        )
                        .child(
                            Button::new("overwrite")
                                .small()
                                .ghost()
                                .label("Keep mine")
                                .on_click(move |_, w, cx| {
                                    e2.update(cx, |e, cx| e.save(true, w, cx))
                                }),
                        ),
                )
            })
            .child(div().flex_1().min_h_0().child(editor))
            .into_any_element()
    }
}
