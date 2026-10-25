//! A changed file opened for editing next to the chat, with optional Vim keys.
use crate::files::fingerprint;
use crate::vim::{self, Vim};
use crate::*;
use gpui_kit::component::input::{
    Editor, EditorState, Indent, InputEvent, MovePageDown, MovePageUp, Outdent, Redo, Search, Undo,
};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{RopeExt as _, WindowExt as _};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// Bigger files open in the diff only; the editor is built for up to ~50K lines.
pub const MAX_BYTES: u64 = 4 << 20;

pub enum EditorEvent {
    /// The file on disk now matches the buffer; the diff should refresh.
    Saved,
    /// Escape with nothing left to cancel: hand focus back to the file tree.
    Leave,
}

pub struct FileEditor {
    /// Diff path, `repo/rel/path`.
    pub path: String,
    abs: PathBuf,
    pub input: Entity<EditorState>,
    /// Fingerprint of the file as last read or written, to notice other writers.
    disk: u64,
    /// Length and mtime as last read; the poll skips reading while they hold.
    stat: Option<(u64, SystemTime)>,
    loaded: bool,
    pub dirty: bool,
    /// The file changed on disk while the buffer had unsaved edits.
    pub conflict: bool,
    pub vim: Vim,
    pub vim_on: bool,
    /// A write is in flight; reloads started before it finished are stale.
    saving: bool,
    saves: u64,
    /// `:wq` is waiting for its write to land.
    close_after_save: bool,
    _subs: Vec<Subscription>,
    _poll: Task<()>,
}

impl EventEmitter<EditorEvent> for FileEditor {}

impl FileEditor {
    pub fn new(
        path: String,
        abs: PathBuf,
        vim_on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let lang = language(&path);
        let input = cx.new(|cx| EditorState::new(window, cx).language(lang));
        let mut subs = vec![
            cx.subscribe(&input, |this: &mut Self, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) && this.loaded && !this.dirty {
                    this.dirty = true;
                    cx.notify();
                }
            }),
        ];
        // Runs before key bindings, so normal-mode keys never reach the editor or the keymap.
        let this = cx.entity().downgrade();
        subs.push(cx.intercept_keystrokes(move |ev, window, cx| {
            let Some(this) = this.upgrade() else { return };
            // The second key of an app chord (`ctrl-w h`) belongs to the keymap, not Vim; when
            // it completes none, the key-down fallback in `render` gets it.
            let handled = !window.has_pending_keystrokes()
                && this.update(cx, |e, cx| e.intercept(&ev.keystroke, window, cx));
            if handled {
                cx.stop_propagation();
            }
        }));
        // The agent writes to the same worktree; notice it within a couple of seconds.
        let _poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                if this.update_in(cx, |e, w, cx| e.reload(w, cx)).is_err() {
                    break;
                }
            }
        });
        let mut editor = FileEditor {
            path,
            abs,
            input,
            disk: 0,
            stat: None,
            loaded: false,
            dirty: false,
            conflict: false,
            vim: Vim::default(),
            vim_on,
            saving: false,
            saves: 0,
            close_after_save: false,
            _subs: subs,
            _poll,
        };
        editor.sync_readonly(cx);
        editor.reload(window, cx);
        editor
    }

    /// Escape out of an idle editor, or a key for Vim; true when handled.
    fn intercept(&mut self, ks: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.is_focused(window, cx) {
            return false;
        }
        if self.idle() && ks.key == "escape" && ks.modifiers.number_of_modifiers() == 0 {
            cx.emit(EditorEvent::Leave);
            return true;
        }
        // Space starts a leader sequence (`space x`), bound under the VimNormal context.
        if self.leader(ks) {
            return false;
        }
        self.vim_on && vim_key(ks).is_some_and(|k| self.vim_key(&k, window, cx))
    }

    fn idle(&self) -> bool {
        !self.vim_on || (self.vim.mode == vim::Mode::Normal && self.vim.pending().is_empty())
    }

    fn leader(&self, ks: &Keystroke) -> bool {
        self.vim_on && self.idle() && ks.key == "space" && ks.modifiers.number_of_modifiers() == 0
    }

    pub fn is_focused(&self, window: &Window, cx: &App) -> bool {
        self.input.read(cx).focus_handle(cx).is_focused(window)
    }

    pub fn set_vim(&mut self, on: bool, cx: &mut Context<Self>) {
        self.vim_on = on;
        self.vim = Vim::default();
        self.sync_readonly(cx);
        cx.notify();
    }

    /// Normal and visual mode must not let stray letters into the buffer.
    fn sync_readonly(&self, cx: &mut Context<Self>) {
        // Until the first read lands, typing would be lost when the file replaces the buffer.
        let ro = !self.loaded || (self.vim_on && self.vim.mode != vim::Mode::Insert);
        self.input.update(cx, |i, cx| i.set_readonly(ro, cx));
    }

    /// Reads the file again: replaces a clean buffer, flags a conflict on a dirty one.
    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let (abs, saves, seen) = (self.abs.clone(), self.saves, self.stat);
        cx.spawn_in(window, async move |this, cx| {
            let read = cx
                .background_executor()
                .spawn(async move {
                    let m = std::fs::metadata(&abs)?;
                    let stat = (m.len(), m.modified()?);
                    // Unchanged, or grown past what the editor handles: stop following it.
                    if Some(stat) == seen || m.len() > MAX_BYTES {
                        return Ok(None);
                    }
                    std::fs::read(&abs).map(|b| Some((stat, b)))
                })
                .await;
            this.update_in(cx, |e, window, cx| {
                if e.saving || e.saves != saves {
                    return; // read before our own write landed
                }
                let Some(read) = read.transpose() else { return };
                e.stat = read.as_ref().ok().map(|(stat, _)| *stat);
                let text = read.map(|(_, b)| b);
                let fp = text.as_ref().map(fingerprint).ok();
                if e.loaded && fp == Some(e.disk) {
                    return;
                }
                // Deleted or no longer UTF-8: keep the buffer and let the user decide.
                let text = text.ok().and_then(|b| String::from_utf8(b).ok());
                match (text, fp) {
                    (Some(_), _) if e.dirty => e.conflict = true,
                    (None, _) => e.conflict = e.loaded,
                    (Some(text), Some(fp)) => {
                        e.disk = fp;
                        e.conflict = false;
                        e.input.update(cx, |i, cx| {
                            let pos = i.cursor_position();
                            i.set_value(text, window, cx);
                            if e.loaded {
                                // Not set_cursor_position: it focuses, stealing the composer's keys.
                                let off = i.text().position_to_offset(&pos);
                                i.set_selected_range(off..off, cx);
                            }
                        });
                        if e.loaded {
                            e.clamp_cursor(cx);
                        } else {
                            e.loaded = true;
                            e.sync_readonly(cx);
                        }
                    }
                    (Some(_), None) => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Throws the edits away and takes the disk version.
    pub fn discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dirty = false;
        self.disk = 0;
        self.stat = None;
        self.reload(window, cx);
    }

    /// Writes the buffer unless someone else changed the file since it was read (then `force`).
    pub fn save(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.loaded || self.saving {
            return;
        }
        self.saving = true;
        let text = self.input.read(cx).value();
        let (abs, expected, saved) = (self.abs.clone(), self.disk, text.clone());
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    // A missing or unreadable file counts as changed: never overwrite blind.
                    let on_disk = std::fs::read(&abs).map(fingerprint);
                    if !force && on_disk.map_or(true, |fp| fp != expected) {
                        return Ok(None);
                    }
                    // In place, not tmp+rename: keeps the file's mode, symlinks and inode.
                    std::fs::write(&abs, text.as_bytes())
                        .map(|_| Some(fingerprint(text.as_bytes())))
                })
                .await;
            this.update_in(cx, |e, window, cx| {
                e.saving = false;
                e.saves += 1;
                let close = std::mem::take(&mut e.close_after_save);
                match result {
                    Ok(Some(fp)) => {
                        e.disk = fp;
                        // Typing during the write keeps the buffer dirty.
                        e.dirty = e.input.read(cx).value() != saved;
                        e.conflict = false;
                        cx.emit(EditorEvent::Saved);
                        if close && !e.dirty {
                            window.dispatch_action(Box::new(CloseFile), cx);
                        }
                    }
                    Ok(None) => e.conflict = true,
                    Err(err) => window.push_notification(
                        Notification::error(format!("Could not save {}: {err}", e.path)),
                        cx,
                    ),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Undo, search and clicks move the cursor without Vim; put it back on a Normal-mode spot.
    fn clamp_cursor(&self, cx: &mut Context<Self>) {
        if !self.vim_on {
            return;
        }
        self.input.update(cx, |i, cx| {
            let c = i.cursor();
            let to = self.vim.clamp(i.text(), c);
            if to != c && i.selected_range().is_empty() {
                i.set_selected_range(to..to, cx);
            }
        });
    }

    /// Runs one key through Vim; returns whether it was consumed.
    fn vim_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let (text, cursor) = {
            let i = self.input.read(cx);
            (i.text().clone(), i.cursor())
        };
        // Only p/P need the clipboard; reading the pasteboard on every key is wasted work.
        let clip = matches!(key, "p" | "P")
            .then(|| cx.read_from_clipboard().and_then(|c| c.text()))
            .flatten();
        let before = self.vim.mode;
        let out = self.vim.key(key, &text, cursor, clip.as_deref());
        if !out.handled {
            return false;
        }
        let edited = out.edit.is_some();
        self.input.update(cx, |i, cx| {
            if let Some((range, s)) = out.edit {
                i.set_selected_range(range, cx);
                i.replace(s, window, cx);
            }
            match (out.select, out.cursor) {
                (Some(sel), _) => i.set_selected_range(sel, cx),
                (None, Some(c)) => i.set_selected_range(c..c, cx),
                (None, None) => {}
            }
        });
        if edited && !self.dirty {
            self.dirty = true;
        }
        if let Some(y) = out.yank {
            cx.write_to_clipboard(ClipboardItem::new_string(y));
        }
        if let Some(cmd) = out.cmd {
            self.run(cmd, window, cx);
        }
        if self.vim.mode != before {
            self.sync_readonly(cx);
        }
        cx.notify();
        true
    }

    fn run(&mut self, cmd: vim::Cmd, window: &mut Window, cx: &mut Context<Self>) {
        use vim::Cmd::*;
        let action: Box<dyn Action> = match cmd {
            Save => return self.save(false, window, cx),
            SaveQuit if self.dirty => {
                self.close_after_save = true;
                return self.save(false, window, cx);
            }
            SaveQuit | Quit => return window.dispatch_action(Box::new(CloseFile), cx),
            ForceQuit => {
                self.dirty = false;
                return window.dispatch_action(Box::new(CloseFile), cx);
            }
            SearchNext | SearchPrev => {
                self.input.update(cx, |i, cx| {
                    let m = match cmd {
                        SearchNext => i.next_search_match(cx),
                        _ => i.previous_search_match(cx),
                    };
                    if let Some(m) = m {
                        i.set_selected_range(m.start..m.start, cx);
                    }
                });
                return self.clamp_cursor(cx);
            }
            Undo => Box::new(self::Undo),
            Redo => Box::new(self::Redo),
            Search => Box::new(self::Search),
            PageDown => Box::new(MovePageDown),
            PageUp => Box::new(MovePageUp),
            Indent => Box::new(self::Indent),
            Outdent => Box::new(self::Outdent),
        };
        // Undo, indent and friends edit even in normal mode.
        self.input.update(cx, |i, cx| i.set_readonly(false, cx));
        window.dispatch_action(action, cx);
        let this = cx.entity().downgrade();
        window.defer(cx, move |_, cx| {
            this.update(cx, |e, cx| {
                e.sync_readonly(cx);
                e.clamp_cursor(cx);
            })
            .ok();
        });
    }
}

pub fn focus(editor: &Entity<FileEditor>, window: &mut Window, cx: &mut App) {
    let input = editor.read(cx).input.clone();
    input.update(cx, |i, cx| i.focus(window, cx));
}

/// The syntect token for a path: its extension, or the file name (`Makefile`).
pub fn language(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext.to_string(),
        _ => name.to_string(),
    }
}

/// The Vim name of a keystroke, or `None` when it belongs to the app (cmd-…, ctrl-w, …).
fn vim_key(ks: &Keystroke) -> Option<String> {
    let m = &ks.modifiers;
    // Nordic and German layouts need Option for `{ } [ ] | ~ @`.
    if m.alt && !(m.platform || m.control || m.function) {
        return ks.key_char.clone().filter(|c| {
            let mut cs = c.chars();
            matches!((cs.next(), cs.next()), (Some(ch), None) if ch.is_ascii_punctuation())
        });
    }
    if m.platform || m.alt || m.function {
        return None;
    }
    if m.control {
        // ctrl-c doesn't stop the agent in the editor; leave insert mode with it, as in Vim.
        if ks.key == "c" {
            return Some("ctrl-[".into());
        }
        return matches!(ks.key.as_str(), "d" | "u" | "r" | "[")
            .then(|| format!("ctrl-{}", ks.key));
    }
    Some(match ks.key.as_str() {
        "escape" | "enter" | "backspace" | "tab" => ks.key.clone(),
        "space" => " ".into(),
        k => match &ks.key_char {
            Some(c) => c.clone(),
            None if m.shift && k.len() == 1 => k.to_uppercase(),
            None if k.chars().count() == 1 => k.to_string(),
            None => return None,
        },
    })
}

impl Render for FileEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A key after a pending prefix that completes no binding (`space j`) skips interceptors;
        // it arrives here once nothing is pending. Keys the interceptor handled never bubble here,
        // and the ones it declined decline again. The replayed `space` itself is dropped: the
        // Editor element resets the state's readonly each render, so it would type.
        div()
            .size_full()
            .on_key_down(cx.listener(|e, ev: &KeyDownEvent, window, cx| {
                let ks = &ev.keystroke;
                if !window.has_pending_keystrokes() && (e.leader(ks) || e.intercept(ks, window, cx))
                {
                    cx.stop_propagation();
                }
            }))
            .child(Editor::new(&self.input).appearance(false).h_full())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn language_tokens() {
        assert_eq!(super::language("api/src/main.rs"), "rs");
        assert_eq!(super::language("api/Makefile"), "Makefile");
        assert_eq!(super::language("api/.env"), ".env");
        assert_eq!(super::language("web/a.test.ts"), "ts");
    }

    #[test]
    fn option_chars_reach_vim() {
        let ks = |s: &str, c: &str| {
            let mut k = gpui_kit::Keystroke::parse(s).unwrap();
            k.key_char = Some(c.into());
            super::vim_key(&k)
        };
        assert_eq!(ks("alt-shift-8", "{").as_deref(), Some("{"));
        assert_eq!(ks("alt-s", "ß"), None);
        assert_eq!(ks("alt-cmd-8", "{"), None);
        let ctrl_c = gpui_kit::Keystroke::parse("ctrl-c").unwrap();
        assert_eq!(super::vim_key(&ctrl_c).as_deref(), Some("ctrl-["));
    }
}

#[cfg(all(test, feature = "test-support"))]
mod ui_tests {
    use super::*;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin
    use gpui_kit::test::{TestAppContextExt, TestWindowExt};
    use std::fs;

    /// A loaded editor on `contents` in a fresh temp dir; returns the dir and the file.
    async fn open_editor(
        tag: &str,
        contents: &str,
        cx: &mut TestAppContext,
    ) -> (PathBuf, PathBuf, AnyWindowHandle, Entity<FileEditor>) {
        let dir = std::env::temp_dir().join(format!("shark-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        fs::write(&file, contents).unwrap();
        cx.update(gpui_kit::init);
        let abs = file.clone();
        let (window, editor) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
                    cx.new(|cx| FileEditor::new("r/a.txt".into(), abs, true, w, cx))
                })
            })
            .unwrap();
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            editor.read(cx).loaded
        })
        .await;
        (dir, file, window, editor)
    }

    #[gpui_kit::test]
    async fn reload_keeps_focus_and_undo_stays_clean(cx: &mut TestAppContext) {
        let (dir, file, window, editor) = open_editor("editor", "one\n", cx).await;
        let text = |cx: &mut TestAppContext| {
            cx.update(|cx| editor.read(cx).input.read(cx).value().to_string())
        };

        // `u` with nothing to undo is not an edit.
        cx.update_window(window, |_, w, cx| {
            focus(&editor, w, cx);
            w.press("u", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|cx| assert!(!editor.read(cx).dirty));

        // An agent write while the user types elsewhere must not pull focus into the file.
        let other = cx.update(|cx| cx.focus_handle());
        cx.update_window(window, |_, w, cx| other.focus(w, cx))
            .unwrap();
        fs::write(&file, "two\n").unwrap();
        cx.executor().advance_clock(Duration::from_secs(3));
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            editor.read(cx).input.read(cx).value().as_ref() == "two\n"
        })
        .await;
        cx.update_window(window, |_, w, cx| {
            assert!(!editor.read(cx).is_focused(w, cx))
        })
        .unwrap();

        // Past the size cap the buffer stops following the file.
        fs::write(&file, "x".repeat(MAX_BYTES as usize + 1)).unwrap();
        cx.executor().advance_clock(Duration::from_secs(3));
        cx.run_until_parked();
        assert_eq!(text(cx), "two\n");
        let _ = fs::remove_dir_all(&dir);
    }

    /// `space j` completes no leader binding: the `j` still moves Vim's cursor.
    #[gpui_kit::test]
    async fn key_after_unbound_leader_reaches_vim(cx: &mut TestAppContext) {
        let (dir, _, window, editor) = open_editor("leader", "one\ntwo\n", cx).await;
        cx.update(|cx| cx.bind_keys([KeyBinding::new("space z", crate::OpenAnyFile, None)]));
        cx.update_window(window, |_, w, cx| {
            focus(&editor, w, cx);
            w.press("space", cx);
            w.press("j", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            let i = editor.read(cx).input.read(cx);
            assert_eq!((i.value().as_ref(), i.cursor()), ("one\ntwo\n", 4));
        });
        let _ = fs::remove_dir_all(&dir);
    }
}
