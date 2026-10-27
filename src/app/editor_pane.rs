//! Full-width file editor tabs.
use super::*;

pub(super) struct FileTab {
    pub(super) editor: Entity<FileEditor>,
    header: (bool, bool, &'static str, String),
    _subs: Vec<Subscription>,
}

impl Shark {
    pub(super) fn open_file(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.open_file_in_tab(ix, false, window, cx);
    }

    pub(super) fn open_file_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.open_file_in_tab(ix, true, window, cx);
    }

    fn open_file_in_tab(
        &mut self,
        ix: usize,
        new_tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(f) = self.diff_files.get(ix) else {
            return;
        };
        let path = f.path().to_string();
        let open = self
            .file_tabs
            .iter()
            .any(|t| t.editor.read(cx).path == path);
        if !open && (f.status() == DiffFileStatus::Deleted || f.is_binary()) {
            return notify_err(
                window,
                cx,
                format!("{path} is deleted or binary; it shows in the diff only"),
            );
        }
        if new_tab {
            self.open_path_tab(path, window, cx);
        } else {
            self.open_path(path, window, cx);
        }
    }

    /// Opens `repo/rel/path`, replacing the selected file after confirming unsaved edits.
    pub(super) fn open_path(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        self.open_path_in_tab(path, false, window, cx);
    }

    pub(super) fn open_path_tab(
        &mut self,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_path_in_tab(path, true, window, cx);
    }

    fn open_path_in_tab(
        &mut self,
        path: String,
        new_tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.active_ws() else { return };
        if let Some(ix) = self
            .file_tabs
            .iter()
            .position(|t| t.editor.read(cx).path == path)
        {
            return self.select_file(ix, window, cx);
        }
        if (self.editing.as_ref()).is_some_and(|e| e.path.as_str() == path && (e.dirty || e.saving))
        {
            return notify_err(window, cx, format!("{path} has unsaved edits in the diff"));
        }
        let abs = ws.dir().join(&path);
        if std::fs::metadata(&abs).is_ok_and(|m| m.len() > crate::editor::MAX_BYTES) {
            return notify_err(
                window,
                cx,
                format!("{path} is too large to edit; it shows in the diff only"),
            );
        }
        let replacing = (!new_tab).then(|| self.editor.clone()).flatten();
        self.confirm_editors(
            replacing.iter().cloned().collect(),
            false,
            window,
            cx,
            move |s, window, cx| {
                let vim = s.state.vim;
                let editor = cx.new(|cx| FileEditor::new(path, abs, vim, window, cx));
                let subs = vec![
                    cx.subscribe_in(
                        &editor,
                        window,
                        |s, editor, ev: &EditorEvent, window, cx| match ev {
                            EditorEvent::Saved => s.refresh_diff(cx),
                            EditorEvent::Close => s.close_editor(editor.clone(), window, cx),
                            EditorEvent::Leave if s.editor.as_ref() == Some(editor) => {
                                let pane = if s.tree_shown() {
                                    Pane::Tree
                                } else {
                                    Pane::Sidebar
                                };
                                s.focus_pane(pane, window, cx)
                            }
                            EditorEvent::Leave => {}
                        },
                    ),
                    cx.observe(&editor, |s, editor, cx| {
                        let Some(tab) = s.file_tabs.iter_mut().find(|t| t.editor == editor) else {
                            return;
                        };
                        let e = editor.read(cx);
                        let header = (
                            e.dirty,
                            e.conflict,
                            e.vim.status(),
                            e.vim.pending().to_string(),
                        );
                        if header != tab.header {
                            tab.header = header;
                            cx.notify();
                        }
                    }),
                ];
                let tab = FileTab {
                    editor: editor.clone(),
                    header: Default::default(),
                    _subs: subs,
                };
                if let Some(ix) = s
                    .file_tabs
                    .iter()
                    .position(|t| Some(&t.editor) == replacing.as_ref())
                {
                    s.file_tabs[ix] = tab;
                } else {
                    s.file_tabs.push(tab);
                }
                s.editor = Some(editor);
                s.show(Tab::File, window, cx);
            },
        );
    }

    pub(super) fn select_file(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.file_tabs.get(ix) else {
            return;
        };
        let path = tab.editor.read(cx).path.clone();
        if self
            .editing
            .as_ref()
            .is_some_and(|e| e.path.as_str() == path && (e.dirty || e.saving))
        {
            return notify_err(window, cx, format!("{path} has unsaved edits in the diff"));
        }
        self.editor = Some(tab.editor.clone());
        self.show(Tab::File, window, cx);
    }

    pub(super) fn close_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.clone() else {
            return;
        };
        self.close_editor(editor, window, cx);
    }

    pub(super) fn close_editor(
        &mut self,
        editor: Entity<FileEditor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.confirm_editors(
            vec![editor.clone()],
            false,
            window,
            cx,
            move |s, window, cx| {
                let Some(ix) = s.file_tabs.iter().position(|t| t.editor == editor) else {
                    return;
                };
                s.file_tabs.remove(ix);
                if s.file_tabs.is_empty() {
                    s.drop_editor();
                    if s.tab == Tab::File {
                        s.show(s.file_fallback(), window, cx);
                    }
                } else if s.editor.as_ref() == Some(&editor) {
                    s.editor = Some(s.file_tabs[ix.min(s.file_tabs.len() - 1)].editor.clone());
                    if s.tab == Tab::File {
                        s.show(Tab::File, window, cx);
                    }
                }
                cx.notify();
            },
        );
    }

    pub(super) fn file_fallback(&self) -> Tab {
        if self.active_ws().is_some_and(|ws| ws.diff_closed) {
            Tab::Chat
        } else {
            Tab::Diff
        }
    }

    pub(super) fn drop_editor(&mut self) {
        self.editor = None;
        self.file_tabs.clear();
    }

    /// Quit, asking first if the open file has unsaved edits.
    pub fn quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_discard(true, true, window, cx, |_, _, cx| cx.quit())
    }

    /// The window's close button: true to close now; with unsaved edits it asks first, and
    /// Discard closes the window.
    pub(super) fn close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let ran = Rc::new(std::cell::Cell::new(false));
        let later = ran.clone();
        self.confirm_discard(true, true, window, cx, move |_, window, _| {
            if later.replace(true) {
                window.remove_window()
            }
        });
        ran.replace(true)
    }

    /// Runs `then` now, or after the user agrees to drop unsaved edits in all file tabs
    /// (`editor`) and of the file edited in the diff (`edit`).
    pub(super) fn confirm_discard(
        &mut self,
        editor: bool,
        edit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Shark, &mut Window, &mut Context<Shark>) + 'static,
    ) {
        let editors = self
            .file_tabs
            .iter()
            .filter(|_| editor)
            .map(|t| t.editor.clone())
            .collect();
        self.confirm_editors(editors, edit, window, cx, then);
    }

    fn confirm_editors(
        &mut self,
        editors: Vec<Entity<FileEditor>>,
        edit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Shark, &mut Window, &mut Context<Shark>) + 'static,
    ) {
        let open = editors
            .iter()
            .filter(|e| e.read(cx).dirty)
            .map(|e| e.read(cx).path.clone());
        // A write under way would land after a Discard; wait for it.
        if let Some(editor) = editors.iter().find(|e| e.read(cx).saving) {
            return window.push_notification(format!("Saving {}…", editor.read(cx).path), cx);
        }
        if let Some(e) = self.editing.as_ref().filter(|e| edit && e.saving) {
            return window.push_notification(format!("Saving {}…", e.path), cx);
        }
        let inline = (self.editing.as_ref())
            .filter(|e| edit && e.dirty)
            .map(|e| e.path.to_string());
        let paths: Vec<String> = open.chain(inline).collect();
        if paths.is_empty() {
            return then(self, window, cx);
        }
        let then = RefCell::new(Some(then));
        confirm(
            format!("Discard changes to {}?", paths.join(" and ")),
            "Your edits are not saved. Cancel and press cmd-s to keep them.",
            "Discard",
            move |s, window, cx| {
                if let Some(f) = then.borrow_mut().take() {
                    for e in &editors {
                        e.update(cx, |e, _| e.dirty = false);
                    }
                    if let Some(e) = s.editing.as_mut().filter(|_| edit) {
                        e.dirty = false;
                    }
                    f(s, window, cx)
                }
            },
            window,
            cx,
        );
    }

    pub(super) fn toggle_vim(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_vim(!self.state.vim, cx);
        let on = self.state.vim;
        window.push_notification(format!("Vim mode {}", if on { "on" } else { "off" }), cx);
    }

    /// Vim mode everywhere: the editor, the composer and the list keys (the root's `Vim`).
    pub(crate) fn set_vim(&mut self, on: bool, cx: &mut Context<Self>) {
        self.state.vim = on;
        self.state.save();
        for tab in &self.file_tabs {
            tab.editor.update(cx, |e, cx| e.set_vim(on, cx));
        }
        for c in self.chats.values() {
            c.update(cx, |c, cx| c.set_vim(on, cx));
        }
        cx.notify();
    }

    /// Right-click menu of the open file: the file's entries, then Close file.
    pub(super) fn editor_menu(&self, cx: &Context<Self>) -> Vec<Entry> {
        let Some(e) = &self.editor else {
            return vec![];
        };
        let mut entries = self.file_menu(&e.read(cx).path.clone(), cx);
        entries.extend([
            Entry::Sep,
            menu::item(
                IconName::Close,
                "Close file",
                on_shark(cx, |s, w, cx| s.close_file(w, cx)),
            ),
        ]);
        entries
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
        // `g t` works in Vim's Normal mode, where it types nothing.
        let normal =
            e.vim_on && e.vim.mode == crate::vim::Mode::Normal && e.vim.pending().is_empty();
        v_flex()
            .id("file-editor-pane")
            .test_support()
            .key_context(if normal {
                "FileEditor VimNormal"
            } else {
                "FileEditor"
            })
            .size_full()
            .child(
                h_flex()
                    .h(px(36.))
                    .flex_none()
                    .px_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_sm()
                    .child(match crate::file_icon::file_icon(&name, theme.is_dark()) {
                        Some(i) => img(i)
                            .size(px(16.))
                            .mx(px(-1.))
                            .flex_none()
                            .into_any_element(),
                        None => Icon::new(IconName::File)
                            .with_size(px(14.))
                            .text_color(theme.muted_foreground)
                            .into_any_element(),
                    })
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(|this, e: &MouseDownEvent, w, cx| {
                                    let entries = this.editor_menu(cx);
                                    this.show_menu(e.position, entries, w, cx);
                                }),
                            )
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
                    .when_some(vim, |d, vim| {
                        d.child(
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
                                .bg(theme.secondary)
                                .hover(|d| d.text_color(theme.foreground))
                                .child(vim)
                                .tooltip(|w, cx| {
                                    Tooltip::new("Turn off Vim mode (ctrl-cmd-v)").build(w, cx)
                                })
                                .on_click(cx.listener(|this, _, w, cx| this.toggle_vim(w, cx))),
                        )
                    })
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
                        crate::settings::close_button("close-file", "Close file (cmd-w)")
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
