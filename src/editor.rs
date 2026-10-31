//! A file opened in a full-width editor tab, with optional Vim keys.
use crate::files::{fingerprint, write_checked};
use crate::vim::{self, Vim};
use crate::*;
use gpui_kit::base::input::{InputBaseState, InputModeKind};
use gpui_kit::component::input::{
    Editor, EditorState, Escape, Indent, InputEvent, MovePageDown, MovePageUp, Outdent, Redo,
    Search, Undo,
};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Rope, RopeExt as _, WindowExt as _};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

mod intelligence;
#[cfg(all(test, feature = "test-support"))]
mod intelligence_tests;
mod snippets;
#[cfg(all(test, feature = "test-support"))]
pub(crate) use intelligence::Symbol;

/// Bigger files open in the diff only; the editor is built for up to ~50K lines.
pub const MAX_BYTES: u64 = 4 << 20;

pub enum EditorEvent {
    /// The file on disk now matches the buffer; the diff should refresh.
    Saved,
    /// Escape with nothing left to cancel: focus the file tree or workspace sidebar.
    Leave,
    /// A completed :wq closes this editor even if another tab is now selected.
    Close,
    Language(crate::lsp::providers::ProviderEvent),
}

/// Code size and line height; rationale in docs/editor-typography.md.
pub const FONT_SIZE: f32 = 14.;
pub const FONT_SIZES: std::ops::RangeInclusive<f32> = 10.0..=24.0;
pub const LINE_HEIGHT: f32 = 1.5;

/// The saved code size (cmd-=/cmd--), shared by the file editor and the diff.
pub struct CodeFont(pub f32);
impl Global for CodeFont {}

/// File editor text size and its whole-pixel row height.
pub fn code_font(cx: &App) -> (Pixels, Pixels) {
    let size = cx.try_global::<CodeFont>().map_or(FONT_SIZE, |f| f.0);
    (px(size), px((size * LINE_HEIGHT).round()))
}

/// Smaller diff text on the same rows gives more breathing room at every zoom level.
pub fn diff_font(cx: &App) -> (Pixels, Pixels) {
    let (size, row) = code_font(cx);
    (size * (13. / 14.), row)
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
    vim_action: bool,
    /// A write is in flight; reloads started before it finished are stale.
    pub(crate) saving: bool,
    saves: u64,
    /// `:wq` is waiting for its write to land.
    close_after_save: bool,
    pub(crate) external: bool,
    pub(crate) intelligence: Option<intelligence::Language>,
    pub(crate) lsp_setup: Option<Task<()>>,
    /// Project discovery is running; this file may be about to join a session.
    pub(crate) discovering: bool,
    /// A dependency source detached by Retry, waiting for this editor's new session.
    pub(crate) reattach_to: Option<WeakEntity<FileEditor>>,
    pub(crate) lsp_request: Option<Task<()>>,
    pub(crate) lsp_error: Option<String>,
    /// Document symbols from the language server, refreshed after each sync.
    pub(crate) outline: Vec<intelligence::Symbol>,
    /// `outline` indices around the cursor, outermost first: the jump bar's path.
    pub(crate) crumbs: Vec<usize>,
    snippets: snippets::Snippets,
    /// Text before and after the last change command (`cw`, `o`, ...): GPUI records its edit
    /// and its typing as two undo steps, Vim as one.
    change: Option<(Rope, Option<Rope>)>,
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
        Self::with_content(path, abs, vim_on, None, window, cx)
    }

    pub(crate) fn with_content(
        path: String,
        abs: PathBuf,
        vim_on: bool,
        content: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let lang = language(&path);
        let input = cx.new(|cx| EditorState::new(window, cx).language(lang));
        let mut subs = vec![cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::Change) && this.loaded {
                    this.lsp_request = None;
                    this.language_changed();
                    this.dirty = true;
                    this.snippet_changed(window, cx);
                    this.sync_language(window, cx);
                    cx.notify();
                }
            },
        )];
        // The cursor moved: the jump bar follows it.
        subs.push(cx.observe(&input, |this: &mut Self, _, cx| this.sync_crumbs(cx)));
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
            vim_action: false,
            saving: false,
            saves: 0,
            close_after_save: false,
            external: false,
            intelligence: None,
            lsp_setup: None,
            discovering: false,
            reattach_to: None,
            lsp_request: None,
            lsp_error: None,
            outline: vec![],
            crumbs: vec![],
            snippets: snippets::Snippets::default(),
            change: None,
            _subs: subs,
            _poll,
        };
        if let Some(text) = content {
            editor.disk = fingerprint(text.as_bytes());
            editor
                .input
                .update(cx, |i, cx| i.set_value(text, window, cx));
            editor.loaded = true;
        }
        editor.sync_readonly(cx);
        editor.reload(window, cx);
        editor
    }

    /// Escape out of an idle editor, or a key for Vim; true when handled.
    fn intercept(&mut self, ks: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.is_focused(window, cx) {
            return false;
        }
        let input = self.input.read(cx);
        let bare_escape = ks.key == "escape" && ks.modifiers.number_of_modifiers() == 0;
        if bare_escape
            && (input.completion_menu_state().open || input.code_action_menu_state().open)
        {
            // The platform menus bubble Escape after dismissal; consume it before it leaves the editor.
            self.input.update(cx, |i, cx| i.dismiss_lsp_overlays(cx));
            return true;
        }
        if self.snippet_key(ks, cx) {
            return true;
        }
        if bare_escape && self.idle() {
            // Let input cancellation run before the bubbled Escape leaves the editor.
            return false;
        }
        self.vim_on && vim_key(ks).is_some_and(|k| self.vim_key(&k, window, cx))
    }

    fn idle(&self) -> bool {
        !self.vim_on || self.vim.idle()
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
        let ro = self.readonly();
        self.input.update(cx, |i, cx| i.set_readonly(ro, cx));
    }

    fn readonly(&self) -> bool {
        self.external || !self.loaded || (self.vim_on && !self.vim.typing() && !self.vim_action)
    }

    /// Reads the file again: replaces a clean buffer, flags a conflict on a dirty one.
    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.read_disk(false, window, cx);
    }

    /// `discard`: take the disk version even over unsaved edits, which stay if the read fails.
    fn read_disk(&mut self, discard: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let seen = if discard { None } else { self.stat };
        let (abs, saves) = (self.abs.clone(), self.saves);
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
                if e.loaded && !discard && fp == Some(e.disk) {
                    return;
                }
                // Deleted or no longer UTF-8: keep the buffer and let the user decide.
                let bad_utf8 =
                    !e.loaded && text.as_ref().is_ok_and(|b| std::str::from_utf8(b).is_err());
                let text = text.ok().and_then(|b| String::from_utf8(b).ok());
                if bad_utf8 {
                    let msg = format!("{} is not UTF-8 text and cannot be edited", e.path);
                    window.push_notification(Notification::error(msg), cx);
                }
                match (text, fp) {
                    (Some(_), _) if e.dirty && !discard => e.conflict = true,
                    (None, _) => e.conflict = e.loaded,
                    (Some(text), Some(fp)) => {
                        e.snippets.resolve = None;
                        e.language_changed();
                        e.disk = fp;
                        e.dirty = false;
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
                        e.sync_language(window, cx);
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
        self.saves += 1; // Invalidate reads started before the discard.
        self.read_disk(true, window, cx);
    }

    /// Writes the buffer unless someone else changed the file since it was read (then `force`).
    pub fn save(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.external || !self.loaded || self.saving {
            return;
        }
        self.saving = true;
        let text = self.input.read(cx).value();
        let (abs, expected, saved) = (self.abs.clone(), self.disk, text.clone());
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { write_checked(&abs, expected, &text, force) })
                .await;
            this.update_in(cx, |e, window, cx| {
                e.saving = false;
                e.saves += 1;
                let close = std::mem::take(&mut e.close_after_save);
                match result {
                    Ok(Some(fp)) => {
                        e.disk = fp;
                        e.stat = None;
                        // Typing during the write keeps the buffer dirty.
                        e.dirty = e.input.read(cx).value() != saved;
                        e.conflict = false;
                        e.language_saved(&saved, window, cx);
                        cx.emit(EditorEvent::Saved);
                        if close && !e.dirty {
                            cx.emit(EditorEvent::Close);
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
        let before = self.vim.mode;
        let text_before = self.input.read(cx).text().clone();
        let Some((edited, cmd)) =
            apply_vim_key(&mut self.vim, &self.input, key, self.external, window, cx)
        else {
            return false;
        };
        self.dirty |= edited;
        if self.vim.mode == vim::Mode::Insert && before != vim::Mode::Insert {
            self.change = edited.then_some((text_before, None));
        } else if before == vim::Mode::Insert {
            let text = self.input.read(cx).text().clone();
            if let Some(c) = &mut self.change {
                c.1 = Some(text);
            }
        } else if edited {
            self.change = None;
        }
        if let Some(cmd) = cmd {
            self.run(cmd, window, cx);
        }
        if self.vim.mode != before {
            self.sync_readonly(cx);
        }
        cx.notify();
        true
    }

    /// The text an undo (or redo) of the last change command ends at, when it takes two steps.
    fn change_target(&self, redo: bool, cx: &App) -> Option<Rope> {
        let (before, after) = self.change.as_ref()?;
        let (from, to) = if redo {
            (before, after.as_ref()?)
        } else {
            (after.as_ref()?, before)
        };
        (self.input.read(cx).text() == from).then(|| to.clone())
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
            NextChat => return window.dispatch_action(Box::new(crate::NextChat), cx),
            PrevChat => return window.dispatch_action(Box::new(crate::PrevChat), cx),
            Outline => return window.dispatch_action(Box::new(crate::DocumentSymbols), cx),
            Definition => {
                return window.dispatch_action(Box::new(gpui_kit::base::input::GoToDefinition), cx);
            }
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
        let indent = matches!(cmd, Indent | Outdent);
        let again = match cmd {
            Undo | Redo => self
                .change_target(cmd == Redo, cx)
                .map(|t| (cmd == Redo, t, 8)),
            _ => None,
        };
        // Undo, indent and friends edit even in normal mode.
        self.vim_action = true;
        self.input.update(cx, |i, cx| i.set_readonly(false, cx));
        window.dispatch_action(action, cx);
        let this = cx.entity().downgrade();
        window.defer(cx, move |window, cx| {
            this.update(cx, |e, cx| e.after_action(indent, again, window, cx))
                .ok();
        });
    }

    /// Steps undo/redo on until the text is the target, then restores Normal mode.
    fn after_action(
        &mut self,
        indent: bool,
        again: Option<(bool, Rope, u8)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((redo, target, left)) = again
            && left > 0
            && *self.input.read(cx).text() != target
        {
            let action: Box<dyn Action> = if redo {
                Box::new(self::Redo)
            } else {
                Box::new(self::Undo)
            };
            window.dispatch_action(action, cx);
            let this = cx.entity().downgrade();
            return window.defer(cx, move |window, cx| {
                let again = Some((redo, target, left - 1));
                this.update(cx, |e, cx| e.after_action(indent, again, window, cx))
                    .ok();
            });
        }
        self.vim_action = false;
        self.sync_readonly(cx);
        if indent {
            // `>>` and `V>` leave the caret on the first line's first non-blank.
            self.input.update(cx, |i, cx| {
                let r = i.selected_range();
                let at = vim::first_nonblank_at(i.text(), r.start.min(r.end));
                i.set_selected_range(at..at, cx);
            });
        }
        self.clamp_cursor(cx);
    }
}

/// Runs one key through `vim` on `input` and applies the edit, cursor, selection and yank.
/// `None` when Vim declined the key; else whether the text changed and the command to run.
/// `readonly` keeps motions, search and yank, and swallows keys that would edit or insert.
pub(crate) fn apply_vim_key<M: InputModeKind>(
    vim: &mut Vim,
    input: &Entity<InputBaseState<M>>,
    key: &str,
    readonly: bool,
    window: &mut Window,
    cx: &mut App,
) -> Option<(bool, Option<vim::Cmd>)> {
    let (text, cursor) = {
        let i = input.read(cx);
        (i.text().clone(), i.cursor())
    };
    // Only p/P need the clipboard; reading the pasteboard on every key is wasted work.
    let clip = matches!(key, "p" | "P")
        .then(|| cx.read_from_clipboard().and_then(|c| c.text()))
        .flatten();
    let before = readonly.then(|| vim.clone());
    let out = vim.key(key, &text, cursor, clip.as_deref());
    if !out.handled {
        return None;
    }
    use crate::vim::Cmd::{Indent, Outdent, Redo, Undo};
    if let Some(before) = before
        && (out.edit.is_some()
            || vim.mode == crate::vim::Mode::Insert
            || matches!(out.cmd, Some(Undo | Redo | Indent | Outdent)))
    {
        // As Vim does for an unmodifiable buffer: no change, and the operator is cancelled.
        *vim = before;
        vim.reset();
        return Some((false, None));
    }
    let edited = out.edit.is_some();
    input.update(cx, |i, cx| {
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
    if let Some(y) = out.yank {
        cx.write_to_clipboard(ClipboardItem::new_string(y));
    }
    Some((edited, out.cmd))
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
pub(crate) fn vim_key(ks: &Keystroke) -> Option<String> {
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
        let (size, row) = code_font(cx);
        // A key after a pending prefix that completes no binding (`ctrl-w j`) skips interceptors;
        // it arrives here once nothing is pending. Keys the interceptor handled never bubble here,
        // and the ones it declined decline again.
        div()
            .size_full()
            .on_action(cx.listener(|e, _: &Escape, window, cx| {
                if e.is_focused(window, cx) && e.idle() {
                    cx.emit(EditorEvent::Leave);
                } else {
                    cx.propagate();
                }
            }))
            // Undo or redo takes back the acceptance a pending auto-import belongs to.
            .capture_action(cx.listener(|e, _: &Undo, _, _| e.snippets.resolve = None))
            .capture_action(cx.listener(|e, _: &Redo, _, _| e.snippets.resolve = None))
            .on_key_down(cx.listener(|e, ev: &KeyDownEvent, window, cx| {
                let ks = &ev.keystroke;
                if !window.has_pending_keystrokes() && e.intercept(ks, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .child(
                Editor::new(&self.input)
                    .readonly(self.readonly())
                    .appearance(false)
                    .h_full()
                    .text_size(size)
                    .line_height(row),
            )
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

    /// A loaded editor on `name` with `contents` in a fresh temp dir; returns the dir and the file.
    pub(super) async fn open_editor(
        name: &str,
        tag: &str,
        contents: &str,
        cx: &mut TestAppContext,
    ) -> (PathBuf, PathBuf, AnyWindowHandle, Entity<FileEditor>) {
        let dir = std::env::temp_dir().join(format!("shark-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join(name);
        fs::write(&file, contents).unwrap();
        cx.update(gpui_kit::init);
        let abs = file.clone();
        let (window, editor) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
                    cx.new(|cx| FileEditor::new(format!("r/{name}"), abs, true, w, cx))
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
        let (dir, file, window, editor) = open_editor("a.txt", "editor", "one\n", cx).await;
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

    /// `ctrl-w j` completes no app chord: the `j` still moves Vim's cursor.
    #[gpui_kit::test]
    async fn key_after_unbound_chord_reaches_vim(cx: &mut TestAppContext) {
        let (dir, _, window, editor) = open_editor("a.txt", "chord", "one\ntwo\n", cx).await;
        cx.update(|cx| cx.bind_keys([KeyBinding::new("ctrl-w z", crate::OpenAnyFile, None)]));
        cx.update_window(window, |_, w, cx| {
            focus(&editor, w, cx);
            w.press("ctrl-w", cx);
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

    /// Only Enter accepts a completion (Tab would indent), so Tab keeps walking the tabstops.
    #[gpui_kit::test]
    async fn tab_with_a_completion_menu_open_moves_to_the_next_tabstop(cx: &mut TestAppContext) {
        use crate::lsp::document::Stop;
        let (dir, _, window, editor) = open_editor("a.py", "tab-menu", "foo(va, y)", cx).await;
        cx.update_window(window, |_, w, cx| {
            editor.update(cx, |e, cx| {
                e.set_vim(false, cx);
                let stops = [(1, 4, 6), (2, 8, 9)];
                e.snippets.active = Some(snippets::Session {
                    text: "foo(va, y)".into(),
                    stops: stops
                        .map(|(index, start, end)| Stop { index, start, end })
                        .into(),
                    index: 1,
                });
                e.input.update(cx, |i, cx| {
                    i.set_selected_range(6..6, cx);
                    let item = lsp_types::CompletionItem {
                        label: "value".into(),
                        ..Default::default()
                    };
                    i.present_completion_items(4, "va", vec![item], cx);
                });
            });
            focus(&editor, w, cx);
            w.render_frame(cx); // the menu hosts its actions once drawn
            w.press("tab", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            let i = editor.read(cx).input.read(cx);
            assert_eq!(i.value().as_ref(), "foo(va, y)");
            assert_eq!(i.selected_value().as_ref(), "y");
        });
        let _ = fs::remove_dir_all(&dir);
    }

    /// Types `text` in Insert mode (`keys` first), then Escape.
    fn type_in(cx: &mut TestAppContext, window: AnyWindowHandle, keys: &[&str], text: &str) {
        cx.update_window(window, |_, w, cx| {
            for k in keys {
                w.press(k, cx);
            }
            w.input(text, cx);
            w.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();
    }

    fn state(
        cx: &mut TestAppContext,
        editor: &Entity<FileEditor>,
    ) -> (String, std::ops::Range<usize>) {
        cx.update(|cx| {
            let i = editor.read(cx).input.read(cx);
            (i.value().to_string(), i.selected_range())
        })
    }

    #[gpui_kit::test]
    async fn failed_discard_keeps_the_edits_marked_dirty(cx: &mut TestAppContext) {
        let (dir, file, window, editor) = open_editor("a.txt", "discard", "one\n", cx).await;
        cx.update_window(window, |_, w, cx| focus(&editor, w, cx))
            .unwrap();
        type_in(cx, window, &["i"], "x");
        cx.update(|cx| assert!(editor.read(cx).dirty));
        // The file is gone: the reload fails, and closing must still ask.
        fs::remove_file(&file).unwrap();
        cx.update_window(window, |_, w, cx| {
            editor.update(cx, |e, cx| e.discard(w, cx))
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            let e = editor.read(cx);
            assert!(e.dirty && e.conflict);
        });
        assert_eq!(state(cx, &editor).0, "xone\n");
        // A readable file is taken over the edits.
        fs::write(&file, "two\n").unwrap();
        cx.update_window(window, |_, w, cx| {
            editor.update(cx, |e, cx| e.discard(w, cx))
        })
        .unwrap();
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            editor.read(cx).input.read(cx).value().as_ref() == "two\n"
        })
        .await;
        cx.update(|cx| {
            let e = editor.read(cx);
            assert!(!e.dirty && !e.conflict);
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    async fn non_utf8_file_says_why_it_is_blank(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("shark-utf8-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        fs::write(&file, [b'a', 0xff, 0xfe]).unwrap();
        cx.update(gpui_kit::init);
        let (window, editor) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
                    cx.new(|cx| FileEditor::new("r/a.txt".into(), file, true, w, cx))
                })
            })
            .unwrap();
        cx.wait_for(window, Duration::from_secs(10), |w, cx| {
            !w.notifications(cx).is_empty()
        })
        .await;
        cx.update(|cx| assert!(!editor.read(cx).loaded));
        let _ = fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    async fn g_t_and_g_shift_t_switch_chats(cx: &mut TestAppContext) {
        use std::{cell::Cell, rc::Rc};
        let (dir, _, window, editor) = open_editor("a.txt", "gt", "one\n", cx).await;
        let seen = Rc::new((Cell::new(0), Cell::new(0)));
        let s = seen.clone();
        cx.update(|cx| {
            cx.on_action(move |_: &crate::NextChat, _| s.0.set(s.0.get() + 1));
            let s = seen.clone();
            cx.on_action(move |_: &crate::PrevChat, _| s.1.set(s.1.get() + 1));
        });
        cx.update_window(window, |_, w, cx| {
            focus(&editor, w, cx);
            w.press("g", cx);
            w.press("t", cx);
            w.press("g", cx);
            w.press("shift-t", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!((seen.0.get(), seen.1.get()), (1, 1));
        let _ = fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    async fn indent_leaves_the_caret_on_the_first_nonblank(cx: &mut TestAppContext) {
        let (dir, _, window, editor) = open_editor("a.txt", "indent", "a\nb\nc\n", cx).await;
        cx.update_window(window, |_, w, cx| {
            focus(&editor, w, cx);
            w.press("j", cx);
            w.press(">", cx);
            w.press(">", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let (text, sel) = state(cx, &editor);
        let at = text.find('b').unwrap();
        assert!(at > 2, "b was indented: {text:?}");
        assert_eq!(sel, at..at);
        // Visual-line: the caret leaves the selection end.
        cx.update_window(window, |_, w, cx| {
            w.press("g", cx);
            w.press("g", cx);
            w.press("V", cx);
            w.press("j", cx);
            w.press(">", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let (text, sel) = state(cx, &editor);
        let at = text.find('a').unwrap();
        assert!(at > 0, "a was indented: {text:?}");
        assert_eq!(sel, at..at);
        let _ = fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    async fn one_undo_reverts_a_whole_change_command(cx: &mut TestAppContext) {
        let (dir, _, window, editor) = open_editor("a.txt", "undo", "foo bar\n", cx).await;
        cx.update_window(window, |_, w, cx| focus(&editor, w, cx))
            .unwrap();
        type_in(cx, window, &["c", "w"], "X");
        assert_eq!(state(cx, &editor).0, "X bar\n");
        cx.update_window(window, |_, w, cx| w.press("u", cx))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(state(cx, &editor).0, "foo bar\n");
        cx.update_window(window, |_, w, cx| w.press("ctrl-r", cx))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(state(cx, &editor).0, "X bar\n");
        cx.update_window(window, |_, w, cx| w.press("u", cx))
            .unwrap();
        cx.run_until_parked();
        type_in(cx, window, &["o"], "new");
        assert_eq!(state(cx, &editor).0, "foo bar\nnew\n");
        cx.update_window(window, |_, w, cx| w.press("u", cx))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(state(cx, &editor).0, "foo bar\n");
        let _ = fs::remove_dir_all(&dir);
    }
}
