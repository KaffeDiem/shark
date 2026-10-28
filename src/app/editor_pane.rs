//! Full-width file editor tabs.
use super::*;

pub(super) struct FileTab {
    pub(super) editor: Entity<FileEditor>,
    header: (bool, bool, &'static str, String, String),
    _subs: Vec<Subscription>,
}

impl Shark {
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
        if (self.differ.read(cx).edit_snapshot())
            .is_some_and(|e| e.path.as_str() == path && (e.dirty || e.saving))
        {
            return notify_err(window, cx, format!("{path} has unsaved edits in the diff"));
        }
        let workspace = ws.name.clone();
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
            true,
            window,
            cx,
            move |s, window, cx| {
                if s.active.as_ref() != Some(&workspace) {
                    return;
                }
                if s.differ
                    .read(cx)
                    .edit_snapshot()
                    .is_some_and(|e| e.path.as_str() == path && (e.dirty || e.saving))
                {
                    return notify_err(window, cx, format!("{path} has unsaved edits in the diff"));
                }
                // Restoration may have opened the target while the discard dialog was up.
                let tab = match s
                    .file_tabs
                    .iter()
                    .position(|t| t.editor.read(cx).path == path)
                {
                    Some(ix) => s.file_tabs.remove(ix),
                    None => s.make_file_tab(path.clone(), abs, None, window, cx),
                };
                let editor = tab.editor.clone();
                if let Some(ix) = s
                    .file_tabs
                    .iter()
                    .position(|t| Some(&t.editor) == replacing.as_ref())
                {
                    let old = TabId::File(s.file_tabs[ix].editor.read(cx).path.clone());
                    s.active_ws_mut()
                        .unwrap()
                        .replace_tab(&old, TabId::File(path));
                    s.file_tabs[ix] = tab;
                } else {
                    s.active_ws_mut().unwrap().open_tab(TabId::File(path));
                    s.file_tabs.push(tab);
                }
                s.sort_file_tabs(cx);
                let _ = s.state.save();
                s.editor = Some(editor);
                s.show(Tab::File, window, cx);
                if let Some(name) = s.active.clone() {
                    s.record_interaction(&name, cx);
                }
            },
        );
    }

    pub(super) fn make_file_tab(
        &mut self,
        path: String,
        abs: PathBuf,
        content: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> FileTab {
        let vim = self.state.vim;
        let editor = cx.new(|cx| match content {
            Some(text) => FileEditor::with_content(path, abs, vim, Some(text), window, cx),
            None => FileEditor::new(path, abs, vim, window, cx),
        });
        self.start_language(&editor, window, cx);
        let subs = vec![
            cx.subscribe_in(
                &editor,
                window,
                |s, editor, ev: &EditorEvent, window, cx| match ev {
                    EditorEvent::Saved => s.refresh_diff(cx),
                    EditorEvent::Close => s.close_editor(editor.clone(), window, cx),
                    EditorEvent::Language(event) => s.language_event(editor, event, window, cx),
                    EditorEvent::Leave if s.editor.as_ref() == Some(editor) => {
                        let pane = if s.tree_shown(cx) {
                            Pane::Feature(Feature::Diff, FeatureFocus::Files)
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
                    e.language_status(cx)
                        .map(|s| format!("{}{:?}", s.0, s.1))
                        .unwrap_or_default(),
                );
                if header != tab.header {
                    tab.header = header;
                    cx.notify();
                }
            }),
        ];
        FileTab {
            editor,
            header: Default::default(),
            _subs: subs,
        }
    }

    pub(super) fn sort_file_tabs(&mut self, cx: &App) {
        let Some(ws) = self.active_ws() else { return };
        let order: Vec<_> = ws.file_tabs().cloned().collect();
        self.file_tabs
            .sort_by_key(|t| order.iter().position(|p| *p == t.editor.read(cx).path));
    }

    /// Restore disk contents in the background without selecting or focusing a file.
    pub(super) fn restore_file_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self
            .active_ws()
            .filter(|ws| ws.file_tabs().next().is_some())
            .cloned()
        else {
            return;
        };
        let generation = self.restore_generation;
        cx.spawn_in(window, async move |this, cx| {
            let name = ws.name.clone();
            let restored = cx
                .background_executor()
                .spawn(async move {
                    ws.file_tabs()
                        .map(|path| (path.clone(), restore_file(&ws, path)))
                        .collect::<Vec<_>>()
                })
                .await;
            this.update_in(cx, |s, window, cx| {
                if s.restore_generation != generation || s.active.as_ref() != Some(&name) {
                    return;
                }
                let mut failures = vec![];
                for (path, result) in restored {
                    // It may have been opened, replaced or closed while the read was running.
                    if !s
                        .active_ws()
                        .unwrap()
                        .tab_order
                        .contains(&TabId::File(path.clone()))
                        || s.file_tabs.iter().any(|t| t.editor.read(cx).path == path)
                    {
                        continue;
                    }
                    match result {
                        Ok((abs, text)) => {
                            let tab = s.make_file_tab(path, abs, Some(text), window, cx);
                            s.file_tabs.push(tab);
                        }
                        Err(e) => failures.push(format!("{path}: {e}")),
                    }
                }
                s.sort_file_tabs(cx);
                s.reveal_tab(cx);
                if !failures.is_empty() {
                    notify_err(
                        window,
                        cx,
                        format!(
                            "Could not reopen files (will retry next visit): {}",
                            failures.join("; ")
                        ),
                    );
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn select_file(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.file_tabs.get(ix) else {
            return;
        };
        let path = tab.editor.read(cx).path.clone();
        if self
            .differ
            .read(cx)
            .edit_snapshot()
            .is_some_and(|e| e.path.as_str() == path && (e.dirty || e.saving))
        {
            return notify_err(window, cx, format!("{path} has unsaved edits in the diff"));
        }
        self.editor = Some(tab.editor.clone());
        self.show(Tab::File, window, cx);
        if let Some(name) = self.active.clone() {
            self.record_interaction(&name, cx);
        }
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
            true,
            window,
            cx,
            move |s, window, cx| {
                let Some(ix) = s.file_tabs.iter().position(|t| t.editor == editor) else {
                    return;
                };
                let path = editor.read(cx).path.clone();
                s.active_ws_mut().unwrap().close_tab(&TabId::File(path));
                let _ = s.state.save();
                s.file_tabs.remove(ix);
                if s.file_tabs.is_empty() {
                    s.editor = None;
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
            Tab::Feature(Feature::Diff)
        }
    }

    pub(super) fn drop_editor(&mut self) {
        self.restore_generation += 1;
        self.editor = None;
        self.file_tabs.clear();
        self.language_sessions.clear();
        self.navigation = Default::default();
    }

    /// Quit, asking first if the open file has unsaved edits.
    pub fn quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.storage_guard(true, window, cx) {
            return;
        }
        let editors = self.file_tabs.iter().map(|t| t.editor.clone()).collect();
        self.confirm_editors(editors, true, false, window, cx, |s, window, cx| {
            // Background saves can fail while the discard dialog is open.
            if s.storage_guard(true, window, cx) {
                cx.quit();
            }
        })
    }

    /// The window's close button: true to close now; with unsaved edits it asks first, and
    /// Discard closes the window.
    pub(super) fn close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.storage_guard(false, window, cx) {
            return false;
        }
        let ran = Rc::new(std::cell::Cell::new(false));
        let later = ran.clone();
        let editors = self.file_tabs.iter().map(|t| t.editor.clone()).collect();
        self.confirm_editors(editors, true, false, window, cx, move |s, window, cx| {
            if !s.storage_guard(false, window, cx) {
                return;
            }
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
        self.confirm_editors(editors, edit, true, window, cx, then);
    }

    pub(super) fn confirm_editors(
        &mut self,
        editors: Vec<Entity<FileEditor>>,
        edit: bool,
        clear_dirty: bool,
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
        if let Some(e) = self
            .differ
            .read(cx)
            .edit_snapshot()
            .filter(|e| edit && e.saving)
        {
            return window.push_notification(format!("Saving {}…", e.path), cx);
        }
        let inline = (self.differ.read(cx).edit_snapshot())
            .filter(|e| edit && e.dirty)
            .map(|e| e.path.to_string());
        let paths: Vec<String> = open.chain(inline).collect();
        if paths.is_empty() {
            return then(self, window, cx);
        }
        let workspace = self.active.clone();
        let inline = self.differ.read(cx).edit_snapshot();
        let buffers: Vec<_> = self
            .file_tabs
            .iter()
            .map(|tab| {
                let editor = &tab.editor;
                let e = editor.read(cx);
                (
                    editor.clone(),
                    e.path.clone(),
                    e.dirty,
                    e.input.read(cx).value().to_string(),
                )
            })
            .collect();
        let then = RefCell::new(Some(then));
        confirm(
            format!("Discard changes to {}?", paths.join(" and ")),
            "Your edits are not saved. Cancel and press cmd-s to keep them.",
            "Discard",
            move |s, window, cx| {
                if let Some(f) = then.borrow_mut().take() {
                    if s.active != workspace
                        || s.file_tabs.iter().any(|t| {
                            let e = t.editor.read(cx);
                            !buffers.iter().any(|(editor, _, _, _)| editor == &t.editor)
                                && (e.dirty || e.saving)
                        })
                        || s.differ.read(cx).edit_snapshot() != inline
                        || buffers.iter().any(|(editor, path, dirty, text)| {
                            let e = editor.read(cx);
                            !s.file_tabs.iter().any(|t| t.editor == *editor)
                                || e.path != *path
                                || e.dirty != *dirty
                                || e.saving
                                || e.input.read(cx).value().as_ref() != text
                        })
                    {
                        return notify_err(
                            window,
                            cx,
                            "The workspace or edits changed; try the action again",
                        );
                    }
                    // Closing may still be canceled by the final storage check.
                    if clear_dirty {
                        for e in &editors {
                            e.update(cx, |e, _| e.dirty = false);
                        }
                        if edit && let Some(expected) = &inline {
                            s.differ.update(cx, |d, _| d.clear_dirty(expected));
                        }
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
        let _ = self.state.save();
        self.configure_differ(cx);
        for tab in &self.file_tabs {
            tab.editor.update(cx, |e, cx| e.set_vim(on, cx));
        }
        for c in self.chats.values() {
            c.update(cx, |c, cx| c.set_vim(on, cx));
        }
        cx.notify();
    }

    /// Right-click menu of the open file: the file's entries, then Close file.
    pub(super) fn editor_menu(&self, cx: &mut Context<Self>) -> Vec<Entry> {
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
        let viewed = self.differ.read(cx).viewed_path(&path);
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
        let language_status = e.language_status(cx);
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
                            .when(e.external, |d| {
                                d.child(
                                    div()
                                        .ml_2()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child("Read only"),
                                )
                            })
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
                    .when_some(language_status.clone(), |d, (status, detail)| {
                        d.child(
                            Button::new("language-status")
                                .small()
                                .ghost()
                                .label(status)
                                .tooltip(detail.unwrap_or_else(|| "Restart language tools".into()))
                                .on_click(
                                    cx.listener(|s, _, w, cx| s.choose_or_restart_language(w, cx)),
                                ),
                        )
                    })
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
                        let path = path.clone();
                        d.child(
                            Checkbox::new("editor-viewed")
                                .label("Viewed")
                                .small()
                                .checked(v == Viewed::Yes)
                                .on_click(cx.listener(move |this, _: &bool, _, cx| {
                                    this.differ
                                        .update(cx, |d, cx| d.toggle_viewed_path(&path, cx))
                                })),
                        )
                    })
                    .child(
                        crate::settings::close_button("close-file", "Close file (cmd-w)")
                            .on_click(cx.listener(|this, _, w, cx| this.close_file(w, cx))),
                    ),
            )
            .when_some(
                language_status.and_then(|(status, detail)| {
                    (status == "Setup needed").then_some(detail).flatten()
                }),
                |d, detail| {
                    let deps = [crate::deps::Dep {
                        name: "Language tools",
                        ok: false,
                        fix: "",
                        detail,
                    }];
                    d.child(
                        div()
                            .px_3()
                            .py_2()
                            .children(crate::deps::render_setup(&deps, cx)),
                    )
                },
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

/// Saved paths are untrusted; only regular UTF-8 text within a workspace repo is editable.
fn restore_file(ws: &Workspace, path: &str) -> Result<(PathBuf, String), String> {
    use std::io::Read;
    use std::path::Component;
    let parts: Vec<_> = Path::new(path).components().collect();
    if parts.len() < 2
        || parts.iter().any(|p| !matches!(p, Component::Normal(_)))
        || path.split('/').any(|p| matches!(p, "" | "." | ".."))
    {
        return Err("invalid repository-relative path".into());
    }
    let repo = parts[0].as_os_str();
    if !ws
        .repos
        .iter()
        .any(|r| std::ffi::OsStr::new(&r.name) == repo)
    {
        return Err("repository is not in this workspace".into());
    }
    let read = || -> Result<_, std::io::Error> {
        let root = ws.dir().join(repo).canonicalize()?;
        let abs = ws.dir().join(path).canonicalize()?;
        if !abs.starts_with(root) {
            return Err(std::io::Error::other("path leaves its repository"));
        }
        let meta = std::fs::metadata(&abs)?;
        if !meta.is_file() || meta.len() > crate::editor::MAX_BYTES {
            return Err(std::io::Error::other("not an editable file or too large"));
        }
        let file = std::fs::File::open(&abs)?;
        let mut bytes = vec![];
        file.take(crate::editor::MAX_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > crate::editor::MAX_BYTES || bytes.contains(&0) {
            return Err(std::io::Error::other("binary or too large"));
        }
        let text = String::from_utf8(bytes).map_err(std::io::Error::other)?;
        // Keep the opened path so polling and save checks still notice symlink changes.
        Ok((ws.dir().join(path), text))
    };
    read().map_err(|e| e.to_string())
}
