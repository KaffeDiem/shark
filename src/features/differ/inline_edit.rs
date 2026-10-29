//! Editing a file's working copy in place in the diff, a line at a time; every edit compares
//! the buffer against the base again, so the diff shows unsaved edits.
use super::*;
use crate::diff_view::{DiffLinePosition, DiffSide};

/// A working copy as lines, and the line the caret is on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Buffer {
    pub(crate) lines: Vec<String>,
    pub(crate) line: usize,
    /// The text ends with a newline.
    eol: bool,
}

impl Buffer {
    pub(crate) fn new(text: &str) -> Self {
        let eol = text.ends_with('\n');
        let body = text.strip_suffix('\n').unwrap_or(text);
        Buffer {
            lines: body.split('\n').map(str::to_string).collect(),
            line: 0,
            eol,
        }
    }

    pub(crate) fn text(&self) -> String {
        let mut text = self.lines.join("\n");
        if self.eol {
            text.push('\n');
        }
        text
    }

    /// The file's indent: a tab if a line starts with one, else its shortest run of two or
    /// more leading spaces, else four spaces.
    pub(crate) fn indent(&self) -> String {
        // Blank lines' whitespace says nothing about the indent.
        let lines = || self.lines.iter().filter(|l| !l.trim().is_empty());
        if lines().any(|l| l.starts_with('\t')) {
            return "\t".into();
        }
        let n = lines()
            .map(|l| l.len() - l.trim_start_matches(' ').len())
            .filter(|&n| n >= 2)
            .min();
        " ".repeat(n.unwrap_or(4))
    }

    /// Backspace at column 0: joins the line to the one above. Returns the caret's column.
    pub(crate) fn join_up(&mut self) -> Option<usize> {
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
    pub(crate) fn insert(&mut self, range: std::ops::Range<usize>, text: &str) -> usize {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let cur = &self.lines[self.line];
        // The input's range can outlive a reload of the line; keep it inside.
        let end = cur.floor_char_boundary(range.end);
        let range = cur.floor_char_boundary(range.start.min(end))..end;
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
pub(crate) fn patch(path: &str, base: &str, text: &str, added: bool) -> String {
    let old = match added {
        true => "/dev/null".to_string(),
        false => format!("a/{path}"),
    };
    // An empty buffer still has its one (empty) line to edit; git shows no row for it.
    let text = if text.is_empty() { "\n" } else { text };
    let mut out = format!("diff --git a/{path} b/{path}\n");
    if added {
        out += "new file mode 100644\n";
    }
    if base == text {
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
pub(crate) fn first_change(base: &str, text: &str) -> usize {
    similar::TextDiff::from_lines(base, text)
        .ops()
        .iter()
        .find(|op| op.tag() != similar::DiffTag::Equal)
        .map_or(0, |op| op.new_range().start)
}

/// One file of the diff open for editing in place.
pub(crate) struct InlineEdit {
    pub(crate) path: SharedString,
    pub(crate) abs: PathBuf,
    /// The original side; every edit is compared against it.
    base: String,
    base_blob: Option<String>,
    added: bool,
    pub(crate) buf: Buffer,
    /// The buffer against `base`, as shown in the diff.
    pub(crate) file: DiffFile,
    /// The file and hunks as git has them, shown again when editing ends.
    pub(crate) orig: (DiffFile, Vec<review::Hunk>),
    pub(crate) patch: String,
    /// Fingerprint of the file as last read or written, to notice other writers.
    disk: u64,
    pub(crate) dirty: bool,
    /// Saved, and not edited since.
    pub(crate) saved: bool,
    /// A save is being written.
    pub(crate) saving: bool,
    /// Something else wrote the file under unsaved edits.
    pub(crate) stale: bool,
    window: AnyWindowHandle,
    pub(crate) input: Entity<InputState>,
    _subs: Vec<Subscription>,
}

impl InlineEdit {
    /// Refresh the original side without changing the unsaved working copy.
    pub(crate) fn rebase(&mut self, ws: &Workspace, file: &DiffFile) -> Result<(), String> {
        if let Some(sha) = original_blob(file)
            && self.base_blob.as_deref() == Some(sha)
        {
            return Ok(());
        }
        let (base, _, added) = load(ws, file)?;
        self.base = base;
        self.base_blob = original_blob(file).map(str::to_string);
        self.added = added;
        self.update_file();
        Ok(())
    }

    fn update_file(&mut self) {
        self.patch = patch(&self.path, &self.base, &self.buf.text(), self.added);
        if let Some(file) = parse(&self.patch) {
            self.file = file;
        }
    }

    pub(crate) fn focused(&self, window: &Window, cx: &App) -> bool {
        self.input.read(cx).focus_handle(cx).is_focused(window)
    }

    /// Edited since the last save.
    fn touch(&mut self) {
        (self.dirty, self.saved) = (true, false);
    }

    pub(crate) fn caret(&self) -> DiffLinePosition {
        DiffLinePosition::new(self.path.clone(), DiffSide::Modified, self.buf.line + 1)
    }
}

fn original_blob(file: &DiffFile) -> Option<&str> {
    file.extended_headers()
        .iter()
        .find_map(|h| h.strip_prefix("index "))
        .and_then(|h| h.split_once(".."))
        .map(|(old, _)| old)
}

/// Reads the base and working text of `file` in `ws`, or why it can't be edited in place.
fn load(ws: &Workspace, file: &DiffFile) -> Result<(String, String, bool), String> {
    let path = file.path();
    if file.status() == DiffFileStatus::Deleted || file.is_binary() {
        return Err(format!("{path} is deleted or binary; it can't be edited"));
    }
    let abs = ws.dir().join(path.as_str());
    // Saving would write through the link, to a file outside the diff.
    if std::fs::symlink_metadata(&abs).is_ok_and(|m| m.is_symlink()) {
        return Err(format!(
            "{path} is a symlink; it can't be edited in the diff"
        ));
    }
    if std::fs::metadata(&abs).is_ok_and(|m| m.len() > crate::editor::MAX_BYTES) {
        return Err(format!("{path} is too large to edit in the diff"));
    }
    let text =
        crate::editor::read_text(&abs).map_err(|e| format!("{path} can't be read as text: {e}"))?;
    let headers = file.extended_headers();
    let added = headers.iter().any(|h| h.starts_with("new file mode"));
    let sha = original_blob(file);
    let base = match sha {
        _ if added => String::new(),
        // A mode-only change: git prints no index line when the content is the same.
        None => text.clone(),
        Some(sha) => {
            let repo = ws.dir().join(path.split('/').next().unwrap_or(""));
            // ponytail: one blocking `git cat-file` on entering edit mode; move it off the UI
            // thread if big repos make it lag.
            let out = std::process::Command::new("git")
                .current_dir(&repo)
                .args(["cat-file", "blob", sha])
                .output()
                .map_err(|e| e.to_string())?;
            if !out.status.success() {
                return Err(format!("{path}: git cat-file failed"));
            }
            String::from_utf8(out.stdout).map_err(|_| format!("{path}: the base isn't text"))?
        }
    };
    // ponytail: CRLF files stay read-only here; keep each line's ending if they need editing.
    if text.contains('\r') || base.contains('\r') {
        return Err(format!(
            "{path} has CRLF line endings; open it with o to edit"
        ));
    }
    Ok((base, text, added))
}

impl Differ {
    /// The diff footer's line while a file is edited in place, for the hint or review bar.
    pub(crate) fn edit_status(&self) -> Option<&'static str> {
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
    pub(crate) fn working_heading(&self, path: &str) -> &'static str {
        match self.editing.as_ref().filter(|e| e.path == path) {
            Some(e) if e.stale => "Working copy · changed on disk",
            Some(e) if e.dirty => "Working copy · unsaved",
            Some(e) if e.saved => "Working copy · saved just now",
            _ => "Working copy · saved",
        }
    }

    /// Edit the working copy of `path`, or of the selected line's file, or of the file the
    /// cursor is on.
    pub(crate) fn edit_working_copy(
        &mut self,
        path: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = self.edit_target(path, window, cx);
        if let (Some(path), Some(workspace)) = (path, self.workspace_name()) {
            cx.emit(DifferEvent::EditWorkingCopy {
                workspace: workspace.into(),
                path,
            });
        }
    }

    /// Called only after the host checks both editors and confirms the captured edit.
    pub(crate) fn begin_edit(
        &mut self,
        path: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.active_ws() else {
            return;
        };
        if self.editing.as_ref().is_some_and(|e| e.path == path) {
            return self.focus_edit(window, cx);
        }
        let Some(file) = self.diff_files.iter().find(|f| *f.path() == path) else {
            return;
        };
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
        {
            let s = self;
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
                base_blob: original_blob(&orig.0).map(str::to_string),
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
        }
    }

    /// Puts the caret's line in the input, the caret at `col` (else the end), and shows the
    /// buffer in the diff.
    fn show_edit_at(&mut self, col: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        self.load_line(col, window, cx);
        self.sync_edit(cx);
        self.focus_edit(window, cx);
    }

    /// Moves the caret to its new line, the text unchanged: no new comparison.
    fn move_caret(&mut self, col: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        self.load_line(col, window, cx);
        self.focus_edit(window, cx);
    }

    /// The caret's line into the input, the caret at `col` (else the end).
    fn load_line(&mut self, col: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(e) = &self.editing else { return };
        let (line, input) = (e.buf.lines[e.buf.line].clone(), e.input.clone());
        input.update(cx, |i, cx| {
            i.set_value(line.clone(), window, cx);
            let col = line.floor_char_boundary(col.unwrap_or(line.len()));
            i.set_selected_range(col..col, cx);
        });
    }

    /// File `abs` as a refresh read it from disk, if it is still the one edited. Written by
    /// someone else: a clean buffer takes the new text, unsaved edits are marked stale (saving
    /// then asks); deleted, it is marked stale.
    pub(crate) fn edit_disk(&mut self, abs: &Path, bytes: Option<Vec<u8>>, cx: &mut Context<Self>) {
        let Some(e) = (self.editing.as_mut()).filter(|e| !e.saving && e.abs == abs) else {
            return;
        };
        let Some(bytes) = bytes else {
            e.stale = true;
            return cx.notify();
        };
        let fp = crate::files::fingerprint(&bytes);
        if fp == e.disk {
            // Back to what was read: saving no longer overwrites anything.
            e.stale = false;
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
        // The new line into the input once this update is over, in the edit's window
        // (refreshes come without one).
        let (w, this) = (e.window, cx.entity());
        cx.defer(move |cx| {
            w.update(cx, |_, window, cx| {
                this.update(cx, |s, cx| s.load_line(None, window, cx))
            })
            .ok();
        });
    }

    /// Focuses the line input, its row brought into view first: an undrawn input loses focus.
    pub(crate) fn focus_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(e) = &self.editing {
            let caret = e.caret();
            self.diff.update(cx, |d, cx| d.show_line(caret, cx));
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
        e.touch();
        self.sync_edit(cx);
    }

    /// Compares the buffer again and shows it in the diff and the tree, keeping the scroll.
    fn sync_edit(&mut self, cx: &mut Context<Self>) {
        let Some(e) = &mut self.editing else { return };
        e.update_file();
        let (file, caret) = (e.file.clone(), e.caret());
        self.review
            .hunks
            .insert(e.path.clone(), review::hunks(&e.file));
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
        // Tab indents by the file's unit, through the input so ⌘Z undoes it; it never moves
        // focus out of the line (and onto the diff's letter keys).
        if ks.key == "tab" && ks.modifiers.number_of_modifiers() == ks.modifiers.shift as u8 {
            if !ks.modifiers.shift {
                let unit = e.buf.indent();
                e.input.update(cx, |i, cx| i.replace(unit, window, cx));
                self.typed(cx);
            }
            return true;
        }
        if ks.modifiers.number_of_modifiers() > 0 {
            return false;
        }
        let (col, range) = {
            let i = e.input.read(cx);
            (i.cursor(), i.selected_range())
        };
        let empty = range.is_empty();
        let col = match ks.key.as_str() {
            // The selection goes, the rest of the line moves to a new line below.
            "enter" => Some(e.buf.insert(range, "\n")),
            "backspace" if col == 0 && empty => match e.buf.join_up() {
                Some(col) => Some(col),
                None => return true,
            },
            "up" if e.buf.line > 0 => {
                e.buf.line -= 1;
                self.move_caret(Some(col), window, cx);
                return true;
            }
            "down" if e.buf.line + 1 < e.buf.lines.len() => {
                e.buf.line += 1;
                self.move_caret(Some(col), window, cx);
                return true;
            }
            "up" | "down" => return true,
            "escape" => {
                self.leave_edit(window, cx);
                return true;
            }
            _ => return false,
        };
        e.touch();
        self.show_edit_at(col, window, cx);
        true
    }

    /// A paste with line breaks inserts lines; one without is left to the input.
    pub(crate) fn paste_lines(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(e) = &mut self.editing else { return };
        let range = e.input.read(cx).selected_range();
        let col = e.buf.insert(range, text);
        e.touch();
        self.show_edit_at(Some(col), window, cx);
    }

    /// Back to reviewing; asks first when there are unsaved edits.
    pub(crate) fn leave_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.request_discard(AfterDiscard::LeaveEdit, window, cx);
    }

    /// Ends in-place editing: the file shows as git has it again, without any discarded
    /// buffer, and its header (the base's blob) is there to edit it again.
    pub(crate) fn end_edit(&mut self, cx: &mut Context<Self>) {
        let Some(e) = self.editing.take() else { return };
        // The next refresh shows git's view of the file, whatever was written while editing.
        self.diff_fp = 0;
        let (file, hunks) = e.orig;
        self.review.hunks.insert(e.path.clone(), hunks);
        if let Some(f) = self.diff_files.iter_mut().find(|f| *f.path() == e.path) {
            *f = file.clone();
        }
        self.diff.update(cx, |d, cx| d.replace_file(file, cx));
    }

    /// Writes the buffer, unless the file changed on disk since it was read (then asks).
    pub(crate) fn save_edit(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        // A second ⌘S while the first writes would find the file changed.
        let Some(e) = self.editing.as_mut().filter(|e| !e.saving) else {
            return;
        };
        e.saving = true;
        let identity = (e.abs.clone(), e.input.entity_id());
        let (abs, expected, text, path) = (e.abs.clone(), e.disk, e.buf.text(), e.path.clone());
        cx.spawn_in(window, async move |this, cx| {
            let saved = text.clone();
            let result = (cx.background_executor())
                .spawn(async move { crate::editor::write_checked(&abs, expected, &text, force) })
                .await;
            this.update_in(cx, |s, window, cx| {
                if !s
                    .editing
                    .as_ref()
                    .is_some_and(|e| (e.abs.clone(), e.input.entity_id()) == identity)
                {
                    return;
                }
                if let Some(e) = s.editing.as_mut() {
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
                        move |s, window, cx| {
                            if s.editing
                                .as_ref()
                                .is_some_and(|e| (e.abs.clone(), e.input.entity_id()) == identity)
                            {
                                s.save_edit(true, window, cx);
                            }
                        },
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
    pub(crate) fn edit_click(
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
            self.move_caret(None, window, cx);
        }
    }

    /// The line input for the caret's line of the edited file, as the diff's working cell.
    pub(crate) fn with_inline_edit(&self, diff: Diff, cx: &Context<Self>) -> Diff {
        let Some(e) = &self.editing else {
            return diff;
        };
        let (path, line, input) = (e.path.clone(), e.buf.line + 1, e.input.clone());
        let mono = cx.theme().mono_font_family.clone();
        let shark = cx.entity().downgrade();
        diff.render_working_cell(move |p, l, window, cx| {
            (*p == path && l == line).then(|| {
                let shark = shark.clone();
                // ponytail: the edited line is plain text, no syntax colors; a highlighted copy
                // behind transparent input text drifts once the input scrolls a long line.
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
                                let Some(text) = item.text().filter(|t| t.contains(['\n', '\r']))
                                else {
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
/// is whether it has unsaved edits, if it is the edited file.
pub(crate) fn edit_buttons(
    file: &DiffFile,
    editing: Option<bool>,
    shark: WeakEntity<Differ>,
) -> AnyElement {
    if file.status() == DiffFileStatus::Deleted || file.is_binary() {
        return div().into_any_element();
    }
    let path = file.path().clone();
    let on = |f: fn(&mut Differ, SharedString, &mut Window, &mut Context<Differ>)| {
        let (shark, path) = (shark.clone(), path.clone());
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            shark
                .update(cx, |s, cx| f(s, path.clone(), window, cx))
                .ok();
        }
    };
    match editing {
        Some(dirty) => h_flex()
            .gap(px(10.))
            .child(
                Button::new("edit-review")
                    .outline()
                    .xsmall()
                    .h(px(28.))
                    .px(px(9.))
                    .rounded(px(6.))
                    .label("Review")
                    .tooltip("Back to reviewing (esc)")
                    .on_click(on(|s, _, w, cx| s.leave_edit(w, cx))),
            )
            .child(
                Button::new("edit-save")
                    .xsmall()
                    .h(px(28.))
                    .px(px(9.))
                    .rounded(px(6.))
                    .label("Save ⌘S")
                    .map(|b| if dirty { b.primary() } else { b.outline() })
                    .on_click(on(|s, _, w, cx| s.save_edit(false, w, cx))),
            )
            .into_any_element(),
        None => Button::new("edit-working-copy")
            .xsmall()
            .h(px(28.))
            .px(px(9.))
            .rounded(px(6.))
            .label("Edit working copy")
            .tooltip("Edit in place (i)")
            .on_click(on(|s, p, w, cx| s.edit_working_copy(Some(p), w, cx)))
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn buffer_splits_joins_and_pastes() {
        let mut b = Buffer::new("one\ntwo\n");
        assert_eq!(b.lines, ["one", "two"]);
        b.insert(1..1, "\n");
        assert_eq!(
            (b.lines.clone(), b.line),
            (vec!["o".into(), "ne".into(), "two".into()], 1)
        );
        // Enter over a selection replaces it.
        let mut s = Buffer::new("abcd\n");
        assert_eq!(s.insert(1..3, "\n"), 0);
        assert_eq!((s.text(), s.line), ("a\nd\n".into(), 1));
        assert_eq!(b.join_up(), Some(1));
        assert_eq!((b.text(), b.line), ("one\ntwo\n".into(), 0));
        assert_eq!(b.join_up(), None);
        b.line = 1;
        assert_eq!(b.insert(1..2, "a\r\nb\nc"), 1);
        assert_eq!((b.text(), b.line), ("one\nta\nb\nco\n".into(), 3));
        assert_eq!(Buffer::new("x").text(), "x");
        assert_eq!(Buffer::new("").lines, [""]);
        assert_eq!(Buffer::new("a\n\tb\n").indent(), "\t");
        assert_eq!(Buffer::new("a\n * c\n  b\n    d\n").indent(), "  ");
        assert_eq!(Buffer::new("a\n").indent(), "    ");
    }

    #[::core::prelude::v1::test]
    fn indent_skips_blank_lines_and_insert_splits_lone_cr() {
        assert_eq!(Buffer::new("a\n  \n\t\n    b\n").indent(), "    ");
        let mut b = Buffer::new("x\n");
        b.insert(1..1, "1\r2\r\n3");
        assert_eq!(b.text(), "x1\n2\n3\n");
        // A range past the line (an input not yet reloaded) stays inside it.
        b.insert(5..9, "!");
        assert_eq!(b.text(), "x1\n2\n3!\n");
    }

    #[::core::prelude::v1::test]
    fn patch_compares_buffer_with_base() {
        let base = "a\nb\nc\n";
        let mut b = Buffer::new(base);
        b.line = 1;
        b.insert(1..1, "\n");
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
        let rows = |p: &str| {
            review::hunks(&DiffFile::parse(p).unwrap()[0])
                .iter()
                .flatten()
                .count()
        };
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
        // An empty or emptied file keeps a working line to edit.
        for (base, added) in [("", true), ("", false), ("a\nb\n", false)] {
            let p = patch("api/f", base, "", added);
            let hunks = review::hunks(&DiffFile::parse(&p).unwrap()[0]);
            assert!(
                hunks.iter().flatten().any(|r| r.0 != b'-' && r.2 == 1),
                "{p}"
            );
        }
    }
}
