//! Editing a file's working copy in place in the diff, a line at a time; every edit compares
//! the buffer against the base again, so the diff shows unsaved edits.
use super::*;
use gpui_kit::component::diff::{DiffLinePosition, DiffSide};

/// A working copy as lines, and the line the caret is on.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Buffer {
    pub(super) lines: Vec<String>,
    pub(super) line: usize,
    /// The text ends with a newline.
    eol: bool,
}

impl Buffer {
    pub(super) fn new(text: &str) -> Self {
        let eol = text.ends_with('\n');
        let body = text.strip_suffix('\n').unwrap_or(text);
        Buffer {
            lines: body.split('\n').map(str::to_string).collect(),
            line: 0,
            eol,
        }
    }

    pub(super) fn text(&self) -> String {
        let mut text = self.lines.join("\n");
        if self.eol {
            text.push('\n');
        }
        text
    }

    /// Enter at byte `col`: the rest of the line moves to a new line below, with the caret.
    pub(super) fn split(&mut self, col: usize) {
        let rest = self.lines[self.line].split_off(col);
        self.line += 1;
        self.lines.insert(self.line, rest);
    }

    /// Backspace at column 0: joins the line to the one above. Returns the caret's column.
    pub(super) fn join_up(&mut self) -> Option<usize> {
        if self.line == 0 {
            return None;
        }
        let line = self.lines.remove(self.line);
        self.line -= 1;
        let col = self.lines[self.line].len();
        self.lines[self.line].push_str(&line);
        Some(col)
    }

    /// Replaces bytes `range` of the caret's line with `text`, which may hold several lines;
    /// the caret ends after it. Returns the caret's column.
    pub(super) fn insert(&mut self, range: std::ops::Range<usize>, text: &str) -> usize {
        let text = text.replace("\r\n", "\n");
        let cur = &self.lines[self.line];
        let after = cur[range.end..].to_string();
        let mut lines = text.split('\n');
        let first = format!("{}{}", &cur[..range.start], lines.next().unwrap_or(""));
        self.lines[self.line] = first;
        for l in lines {
            self.line += 1;
            self.lines.insert(self.line, l.to_string());
        }
        let col = self.lines[self.line].len();
        self.lines[self.line].push_str(&after);
        col
    }
}

/// `text` against `base` as a git patch of `path`, with every line as context (the diff
/// folds it), so any line can be edited, even with no changes left.
pub(super) fn patch(path: &str, base: &str, text: &str, added: bool) -> String {
    let old = match added {
        true => "/dev/null".to_string(),
        false => format!("a/{path}"),
    };
    let mut out = format!("diff --git a/{path} b/{path}\n");
    if added {
        out += "new file mode 100644\n";
    }
    if base == text && !text.is_empty() {
        // No hunk without a change: one of context only.
        let n = text.lines().count();
        out += &format!("--- {old}\n+++ b/{path}\n@@ -1,{n} +1,{n} @@\n");
        text.lines().for_each(|l| out += &format!(" {l}\n"));
        if !text.ends_with('\n') {
            out += "\\ No newline at end of file\n";
        }
        return out;
    }
    // A deadline keeps a keystroke in a big file from stalling on a worst-case diff.
    let diff = similar::TextDiff::configure()
        .timeout(std::time::Duration::from_millis(30))
        .diff_lines(base, text);
    out += &diff
        .unified_diff()
        .context_radius(base.lines().count().max(text.lines().count()))
        .header(&old, &format!("b/{path}"))
        .to_string();
    out
}

fn parse(patch: &str) -> Option<DiffFile> {
    DiffFile::parse(patch).ok()?.into_iter().next()
}

/// The first line (0-based) of `text` that differs from `base`, or 0.
pub(super) fn first_change(base: &str, text: &str) -> usize {
    similar::TextDiff::from_lines(base, text)
        .ops()
        .iter()
        .find(|op| op.tag() != similar::DiffTag::Equal)
        .map_or(0, |op| op.new_range().start)
}

/// One file of the diff open for editing in place.
pub(super) struct InlineEdit {
    pub(super) path: SharedString,
    pub(super) abs: PathBuf,
    /// The original side; every edit is compared against it.
    base: String,
    added: bool,
    pub(super) buf: Buffer,
    /// The buffer against `base`, as shown in the diff.
    pub(super) file: DiffFile,
    /// The file and hunks as git has them, shown again when editing ends.
    pub(super) orig: (DiffFile, Vec<review::Hunk>),
    pub(super) patch: String,
    /// Fingerprint of the file as last read or written, to notice other writers.
    disk: u64,
    pub(super) dirty: bool,
    /// Saved, and not edited since.
    pub(super) saved: bool,
    /// A save is being written.
    saving: bool,
    /// Something else wrote the file under unsaved edits.
    pub(super) stale: bool,
    window: AnyWindowHandle,
    pub(super) input: Entity<InputState>,
    _subs: Vec<Subscription>,
}

impl InlineEdit {
    fn update_file(&mut self) {
        self.patch = patch(&self.path, &self.base, &self.buf.text(), self.added);
        if let Some(file) = parse(&self.patch) {
            self.file = file;
        }
    }

    pub(super) fn focused(&self, window: &Window, cx: &App) -> bool {
        self.input.read(cx).focus_handle(cx).is_focused(window)
    }

    pub(super) fn caret(&self) -> DiffLinePosition {
        DiffLinePosition::new(self.path.clone(), DiffSide::Modified, self.buf.line + 1)
    }
}

/// Reads the base and working text of `file` in `ws`, or why it can't be edited in place.
fn load(ws: &Workspace, file: &DiffFile) -> Result<(String, String, bool), String> {
    let path = file.path();
    if file.status() == DiffFileStatus::Deleted || file.is_binary() {
        return Err(format!("{path} is deleted or binary; it can't be edited"));
    }
    let abs = ws.dir().join(path.as_str());
    if std::fs::metadata(&abs).is_ok_and(|m| m.len() > crate::editor::MAX_BYTES) {
        return Err(format!("{path} is too large to edit in the diff"));
    }
    let text = std::fs::read(&abs)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or(format!("{path} can't be read as text"))?;
    // ponytail: CRLF files stay read-only here; keep each line's ending if they need editing.
    if text.contains('\r') {
        return Err(format!(
            "{path} has CRLF line endings; open it with o to edit"
        ));
    }
    let sha = (file.extended_headers().iter())
        .find_map(|h| h.strip_prefix("index "))
        .and_then(|h| h.split_once(".."))
        .map(|(old, _)| old.to_string())
        .unwrap_or_default();
    let added = sha.is_empty() || sha.bytes().all(|b| b == b'0');
    let base = match added {
        true => String::new(),
        false => {
            let repo = ws.dir().join(path.split('/').next().unwrap_or(""));
            // ponytail: one blocking `git cat-file` on entering edit mode; move it off the UI
            // thread if big repos make it lag.
            let out = std::process::Command::new("git")
                .current_dir(&repo)
                .args(["cat-file", "blob", &sha])
                .output()
                .map_err(|e| e.to_string())?;
            if !out.status.success() {
                return Err(format!("{path}: git cat-file failed"));
            }
            String::from_utf8(out.stdout).map_err(|_| format!("{path}: the base isn't text"))?
        }
    };
    Ok((base, text, added))
}

impl Shark {
    /// The diff footer's line while a file is edited in place, for the hint or review bar.
    pub(super) fn edit_status(&self) -> Option<&'static str> {
        let e = self.editing.as_ref()?;
        Some(match e.saved && !e.dirty {
            _ if e.stale => {
                "The file changed on disk · ⌘S asks before overwriting · esc then i reloads it"
            }
            true => {
                "Saved · Comparison updated in place · Divider widths and scroll position retained"
            }
            false => "Editing the working copy · Comparison includes unsaved edits · ⌘S saves",
        })
    }

    /// The working-copy column heading of `path`.
    pub(super) fn working_heading(&self, path: &str) -> &'static str {
        match self.editing.as_ref().filter(|e| e.path == path) {
            Some(e) if e.stale => "Working copy · changed on disk",
            Some(e) if e.dirty => "Working copy · unsaved",
            Some(e) if e.saved => "Working copy · saved just now",
            _ => "Working copy · saved",
        }
    }

    /// Edit the working copy of `path`, or of the file the cursor is on.
    pub(super) fn edit_working_copy(
        &mut self,
        path: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = path.or_else(|| {
            let ix = self.cursor_file(window, cx)?;
            Some(self.diff_files[ix].path().clone())
        });
        let (Some(path), Some(ws)) = (path, self.active_ws()) else {
            return;
        };
        if self.editing.as_ref().is_some_and(|e| e.path == path) {
            return self.focus_edit(window, cx);
        }
        let Some(file) = self.diff_files.iter().find(|f| *f.path() == path) else {
            return;
        };
        if (self.editor.as_ref()).is_some_and(|e| e.read(cx).path == path && e.read(cx).dirty) {
            return notify_err(
                window,
                cx,
                format!("{path} has unsaved edits in the editor"),
            );
        }
        let (base, text, added) = match load(ws, file) {
            Ok(loaded) => loaded,
            Err(e) => return notify_err(window, cx, e),
        };
        let abs = ws.dir().join(path.as_str());
        let mut buf = Buffer::new(&text);
        // The caret starts on the selected working line, else the first change.
        let selected = (self.diff.read(cx).selected_lines())
            .filter(|r| *r.path() == path && r.side() == DiffSide::Modified)
            .map(|r| r.start().saturating_sub(1));
        buf.line =
            (selected.unwrap_or_else(|| first_change(&base, &text))).min(buf.lines.len() - 1);
        let orig = (
            file.clone(),
            (self.review.hunks.get(&path).cloned()).unwrap_or_default(),
        );
        let patch = patch(&path, &base, &text, added);
        let Some(file) = parse(&patch) else {
            return notify_err(window, cx, format!("{path} can't be compared"));
        };
        self.confirm_discard(false, true, window, cx, move |s, window, cx| {
            let input = cx.new(|cx| InputState::new(window, cx));
            let shark = cx.entity().downgrade();
            let subs = vec![
                cx.subscribe_in(&input, window, |s, _, ev: &InputEvent, _, cx| {
                    if matches!(ev, InputEvent::Change) {
                        s.typed(cx)
                    }
                }),
                // Before the keymap, so the input's own enter/arrow handling never runs.
                cx.intercept_keystrokes(move |ev, window, cx| {
                    let handled = shark
                        .update(cx, |s, cx| s.edit_key(&ev.keystroke, window, cx))
                        .unwrap_or(false);
                    if handled {
                        cx.stop_propagation();
                    }
                }),
            ];
            s.end_edit(cx);
            s.editing = Some(InlineEdit {
                path,
                abs,
                disk: crate::files::fingerprint(text.as_bytes()),
                base,
                added,
                buf,
                file,
                orig,
                patch,
                dirty: false,
                saved: false,
                saving: false,
                stale: false,
                window: window.window_handle(),
                input,
                _subs: subs,
            });
            s.show_edit_at(None, window, cx);
            // The caret's row to the top: revealing it at the bottom would go by the hinted
            // heights of rows never drawn and could miss, and an undrawn input loses focus.
            if let Some(caret) = s.editing.as_ref().map(InlineEdit::caret) {
                s.diff.update(cx, |d, cx| d.scroll_to_line(caret, cx));
            }
        });
    }

    /// Puts the caret's line in the input, the caret at `col` (else the end), and shows the
    /// buffer in the diff.
    fn show_edit_at(&mut self, col: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        self.load_line(col, window, cx);
        self.sync_edit(cx);
        self.focus_edit(window, cx);
    }

    /// The caret's line into the input, the caret at `col` (else the end).
    fn load_line(&mut self, col: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(e) = &self.editing else { return };
        let (line, input) = (e.buf.lines[e.buf.line].clone(), e.input.clone());
        input.update(cx, |i, cx| {
            i.set_value(line.clone(), window, cx);
            let mut col = col.unwrap_or(line.len()).min(line.len());
            while !line.is_char_boundary(col) {
                col -= 1;
            }
            i.set_selected_range(col..col, cx);
        });
    }

    /// Runs `f` with the edit's window once the current update is over (refreshes come
    /// without one).
    pub(super) fn with_edit_window(
        &self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Shark, &mut Window, &mut Context<Shark>) + 'static,
    ) {
        let Some(w) = self.editing.as_ref().map(|e| e.window) else {
            return;
        };
        let this = cx.entity();
        cx.defer(move |cx| {
            w.update(cx, |_, window, cx| this.update(cx, |s, cx| f(s, window, cx)))
                .ok();
        });
    }

    /// The edited file as a refresh read it from disk. Written by someone else: a clean
    /// buffer takes the new text, unsaved edits are marked stale (saving then asks).
    pub(super) fn edit_disk(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        let Some(e) = self.editing.as_mut().filter(|e| !e.saving) else {
            return;
        };
        let fp = crate::files::fingerprint(&bytes);
        if fp == e.disk {
            return;
        }
        let text = String::from_utf8(bytes).ok().filter(|t| !t.contains('\r'));
        let (false, Some(text)) = (e.dirty, text) else {
            e.stale = true;
            return cx.notify();
        };
        let line = e.buf.line;
        e.buf = Buffer::new(&text);
        e.buf.line = line.min(e.buf.lines.len() - 1);
        (e.disk, e.stale, e.saved) = (fp, false, false);
        e.update_file();
        self.with_edit_window(cx, |s, window, cx| s.load_line(None, window, cx));
    }

    pub(super) fn focus_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(e) = &self.editing {
            e.input.read(cx).focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    /// The input changed: take its line into the buffer.
    fn typed(&mut self, cx: &mut Context<Self>) {
        let Some(e) = &mut self.editing else { return };
        let value = e.input.read(cx).value().to_string();
        if e.buf.lines[e.buf.line] == value {
            return;
        }
        e.buf.lines[e.buf.line] = value;
        e.dirty = true;
        e.saved = false;
        self.sync_edit(cx);
    }

    /// Compares the buffer again and shows it in the diff and the tree, keeping the scroll.
    fn sync_edit(&mut self, cx: &mut Context<Self>) {
        let Some(e) = &mut self.editing else { return };
        e.update_file();
        let (file, caret) = (e.file.clone(), e.caret());
        self.review
            .hunks
            .insert(e.path.clone(), review::parse_hunks(&e.patch));
        if let Some(f) = self.diff_files.iter_mut().find(|f| f.path() == file.path()) {
            *f = file.clone();
        }
        self.diff.update(cx, |d, cx| {
            d.replace_file(file, cx);
            d.show_line(caret, cx);
        });
        cx.notify();
    }

    /// Enter, backspace at column 0, the arrows and escape in the line input; true if handled.
    fn edit_key(&mut self, ks: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(e) = self.editing.as_mut().filter(|e| e.focused(window, cx)) else {
            return false;
        };
        // Tab indents; it never moves focus out of the line (and onto the diff's letter keys).
        if ks.key == "tab" && ks.modifiers.number_of_modifiers() == ks.modifiers.shift as u8 {
            if !ks.modifiers.shift {
                self.paste_lines("\t", window, cx);
            }
            return true;
        }
        if ks.modifiers.number_of_modifiers() > 0 {
            return false;
        }
        let (col, empty) = {
            let i = e.input.read(cx);
            (i.cursor(), i.selected_range().is_empty())
        };
        let col = match ks.key.as_str() {
            "enter" => {
                e.buf.split(col);
                Some(0)
            }
            "backspace" if col == 0 && empty => match e.buf.join_up() {
                Some(col) => Some(col),
                None => return true,
            },
            "up" if e.buf.line > 0 => {
                e.buf.line -= 1;
                self.show_edit_at(Some(col), window, cx);
                return true;
            }
            "down" if e.buf.line + 1 < e.buf.lines.len() => {
                e.buf.line += 1;
                self.show_edit_at(Some(col), window, cx);
                return true;
            }
            "up" | "down" => return true,
            "escape" => {
                self.leave_edit(window, cx);
                return true;
            }
            _ => return false,
        };
        e.dirty = true;
        e.saved = false;
        self.show_edit_at(col, window, cx);
        true
    }

    /// A paste with line breaks inserts lines; one without is left to the input.
    pub(super) fn paste_lines(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(e) = &mut self.editing else { return };
        let range = e.input.read(cx).selected_range();
        let col = e.buf.insert(range, text);
        e.dirty = true;
        e.saved = false;
        self.show_edit_at(Some(col), window, cx);
    }

    /// Back to reviewing; asks first when there are unsaved edits.
    pub(super) fn leave_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_discard(false, true, window, cx, |s, window, cx| {
            s.end_edit(cx);
            // git's view of the file, whatever was written while editing.
            s.diff_fp = 0;
            s.refresh_diff(cx);
            s.focus_diff(window, cx);
            cx.notify();
        });
    }

    /// Ends in-place editing: the file shows as git has it again, without any discarded
    /// buffer, and its header (the base's blob) is there to edit it again.
    fn end_edit(&mut self, cx: &mut Context<Self>) {
        let Some(e) = self.editing.take() else { return };
        let (file, hunks) = e.orig;
        self.review.hunks.insert(e.path.clone(), hunks);
        if let Some(f) = self.diff_files.iter_mut().find(|f| *f.path() == e.path) {
            *f = file.clone();
        }
        self.diff.update(cx, |d, cx| d.replace_file(file, cx));
    }

    /// ⌘S / File › Save: the file edited in the diff while it shows, else the editor's.
    pub(super) fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == Tab::Diff && self.editing.is_some() {
            return self.save_edit(false, window, cx);
        }
        if let Some(e) = self.editor.clone() {
            e.update(cx, |e, cx| e.save(false, window, cx))
        }
    }

    /// Writes the buffer, unless the file changed on disk since it was read (then asks).
    pub(super) fn save_edit(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        // A second ⌘S while the first writes would find the file changed.
        let Some(e) = self.editing.as_mut().filter(|e| !e.saving) else {
            return;
        };
        e.saving = true;
        let (abs, expected, text, path) = (e.abs.clone(), e.disk, e.buf.text(), e.path.clone());
        cx.spawn_in(window, async move |this, cx| {
            let saved = text.clone();
            let result = (cx.background_executor())
                .spawn(async move { crate::editor::write_checked(&abs, expected, &text, force) })
                .await;
            this.update_in(cx, |s, window, cx| {
                if let Some(e) = s.editing.as_mut().filter(|e| e.path == path) {
                    e.saving = false;
                }
                match result {
                    Ok(Some(fp)) => {
                        if let Some(e) = s.editing.as_mut().filter(|e| e.path == path) {
                            e.disk = fp;
                            // Typing during the write keeps the buffer dirty.
                            e.dirty = e.buf.text() != saved;
                            (e.saved, e.stale) = (true, false);
                        }
                        s.refresh_diff(cx);
                    }
                    Ok(None) => confirm(
                        format!("{path} changed on disk"),
                        "Someone else wrote the file since you started editing.",
                        "Overwrite",
                        |s, window, cx| s.save_edit(true, window, cx),
                        window,
                        cx,
                    ),
                    Err(err) => notify_err(window, cx, format!("Could not save {path}: {err}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A click on a working-copy line of the edited file puts the caret there.
    pub(super) fn edit_click(
        &mut self,
        pos: &DiffLinePosition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(e) = self.editing.as_mut().filter(|e| e.path == *pos.path()) else {
            return;
        };
        // A click in the line's own input is the input's (caret, selection).
        if pos.side() == DiffSide::Modified
            && pos.line() <= e.buf.lines.len()
            && pos.line() != e.buf.line + 1
        {
            e.buf.line = pos.line() - 1;
            self.show_edit_at(None, window, cx);
        }
    }

    /// The line input for the caret's line of the edited file, as the diff's working cell.
    pub(super) fn with_inline_edit(&self, diff: Diff, cx: &Context<Self>) -> Diff {
        let Some(e) = &self.editing else {
            return diff;
        };
        let (path, line, input) = (e.path.clone(), e.buf.line + 1, e.input.clone());
        let mono = cx.theme().mono_font_family.clone();
        let shark = cx.entity().downgrade();
        diff.render_working_cell(move |p, l, window, cx| {
            (*p == path && l == line).then(|| {
                let shark = shark.clone();
                // The code's font and size, without the input's padding: the text stays put.
                let size = window.rem_size() * (f32::from(cx.theme().mono_font_size) / 16.);
                div()
                    .key_context("InlineEdit")
                    .w_full()
                    .h(size * 1.6)
                    .child(
                        Input::new(&input)
                            .appearance(false)
                            .xsmall()
                            .p_0()
                            .h(size * 1.6)
                            .line_height(size * 1.6)
                            .text_size(size)
                            .font_family(mono.clone())
                            .on_paste(move |item, window, cx| {
                                let Some(text) = item.text().filter(|t| t.contains('\n')) else {
                                    return false;
                                };
                                // The input is mid-update; edit the buffer once it's done.
                                let shark = shark.clone();
                                window.defer(cx, move |window, cx| {
                                    shark
                                        .update(cx, |s, cx| s.paste_lines(&text, window, cx))
                                        .ok();
                                });
                                true
                            }),
                    )
                    .into_any_element()
            })
        })
    }
}

/// A file header's [Edit working copy], or [Review] [Save ⌘S] while it is edited; `editing`
/// is the edited path and whether it has unsaved edits.
pub(super) fn edit_buttons(
    file: &DiffFile,
    editing: Option<&(SharedString, bool)>,
    shark: WeakEntity<Shark>,
) -> AnyElement {
    if file.status() == DiffFileStatus::Deleted || file.is_binary() {
        return div().into_any_element();
    }
    let path = file.path().clone();
    let on = |f: fn(&mut Shark, SharedString, &mut Window, &mut Context<Shark>)| {
        let (shark, path) = (shark.clone(), path.clone());
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            shark
                .update(cx, |s, cx| f(s, path.clone(), window, cx))
                .ok();
        }
    };
    match editing.filter(|(p, _)| *p == path) {
        Some((_, dirty)) => h_flex()
            .gap_1()
            .child(
                Button::new("edit-review")
                    .outline()
                    .xsmall()
                    .label("Review")
                    .tooltip("Back to reviewing (esc)")
                    .on_click(on(|s, _, w, cx| s.leave_edit(w, cx))),
            )
            .child(
                Button::new("edit-save")
                    .xsmall()
                    .label("Save ⌘S")
                    .map(|b| if *dirty { b.primary() } else { b.outline() })
                    .on_click(on(|s, _, w, cx| s.save_edit(false, w, cx))),
            )
            .into_any_element(),
        None => Button::new("edit-working-copy")
            .xsmall()
            .label("Edit working copy")
            .tooltip("Edit in place (i)")
            .on_click(on(|s, p, w, cx| s.edit_working_copy(Some(p), w, cx)))
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin

    #[test]
    fn buffer_splits_joins_and_pastes() {
        let mut b = Buffer::new("one\ntwo\n");
        assert_eq!(b.lines, ["one", "two"]);
        b.split(1);
        assert_eq!(
            (b.lines.clone(), b.line),
            (vec!["o".into(), "ne".into(), "two".into()], 1)
        );
        assert_eq!(b.join_up(), Some(1));
        assert_eq!((b.text(), b.line), ("one\ntwo\n".into(), 0));
        assert_eq!(b.join_up(), None);
        b.line = 1;
        assert_eq!(b.insert(1..2, "a\r\nb\nc"), 1);
        assert_eq!((b.text(), b.line), ("one\nta\nb\nco\n".into(), 3));
        assert_eq!(Buffer::new("x").text(), "x");
        assert_eq!(Buffer::new("").lines, [""]);
    }

    #[test]
    fn patch_compares_buffer_with_base() {
        let base = "a\nb\nc\n";
        let mut b = Buffer::new(base);
        b.line = 1;
        b.split(1);
        let p = patch("api/f", base, &b.text(), false);
        assert!(p.starts_with("diff --git a/api/f b/api/f\n--- a/api/f\n+++ b/api/f\n"));
        let file = DiffFile::parse(&p).unwrap().remove(0);
        assert_eq!(file.path().as_str(), "api/f");
        // Enter at the end of `b` adds an empty line after it.
        assert_eq!((file.additions(), file.deletions()), (1, 0));
        assert_eq!(first_change(base, &b.text()), 2);

        let added = DiffFile::parse(&patch("api/n", "", "x\n", true))
            .unwrap()
            .remove(0);
        assert_eq!(added.status(), DiffFileStatus::Added);
        assert_eq!(first_change("same\n", "same\n"), 0);

        // Every line is there to edit: far from the change, and with no change at all.
        let rows = |p: &str| review::parse_hunks(p).iter().flatten().count();
        let long: String = (1..=20).map(|i| format!("{i}\n")).collect();
        assert_eq!(
            rows(&patch("api/f", &long, &long.replace("20\n", "x\n"), false)),
            21
        );
        for text in ["a\nb\n", "a\nb"] {
            let p = patch("api/f", text, text, false);
            assert_eq!(rows(&p), 2, "{p}");
            assert_eq!(DiffFile::parse(&p).unwrap()[0].additions(), 0);
        }
    }
}
