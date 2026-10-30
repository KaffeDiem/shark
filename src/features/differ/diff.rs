//! The Diff tab and the file tree: loading, viewed marks, tree navigation.
use super::*;
use crate::diff_view::DiffHunkSeparator;
use gpui_kit::component::Selectable as _;

/// A changed file against the fingerprint stored when it was marked viewed.
#[derive(PartialEq, Clone, Copy, Debug)]
pub(crate) enum Viewed {
    No,
    Yes,
    /// Marked viewed, then the agent or the editor changed it.
    Changed,
}

/// One parsed diff refresh, built off the main thread.
pub(crate) struct DiffData {
    /// Fingerprint of the whole patch; equal means nothing to redo.
    fp: u64,
    bases: HashMap<String, String>,
    /// Per repo, the commit the patch is against.
    revs: HashMap<String, String>,
    /// In tree order.
    pub(crate) files: Vec<DiffFile>,
    fps: Vec<u64>,
    /// git failed for some repo; the patch is then empty, which must not read as "no changes".
    error: Option<String>,
    uncommitted: HashSet<String>,
    hunks: HashMap<SharedString, Vec<review::Hunk>>,
    media: Vec<media::MediaFile>,
    /// The file edited in place, and its bytes as on disk when the refresh read it (none:
    /// gone or unreadable).
    edited: Option<(PathBuf, Option<Vec<u8>>)>,
}

/// The file of one patch chunk. One that fails to parse is shown with the reason in its
/// place, not dropped without a word.
fn parse_chunk(chunk: &str) -> Option<DiffFile> {
    match DiffFile::parse(chunk) {
        Ok(files) => files.into_iter().next(),
        Err(e) => {
            let header = chunk.lines().next().unwrap_or_default();
            let path = header.strip_prefix("diff --git ").unwrap_or(header);
            let path = path.rsplit_once(" b/").map_or(path, |(_, b)| b);
            Some(DiffFile::unchanged(
                path,
                &format!("Could not parse this file's diff: {e}\n"),
            ))
        }
    }
}

impl DiffData {
    pub(crate) fn load(ws: &Workspace, uncommitted_only: bool) -> DiffData {
        let (patch, mut error) = match git::combined_diff(ws, &ws.dir(), uncommitted_only) {
            Ok(p) => (p, None),
            Err(e) => (String::new(), Some(e)),
        };
        let fp = files::fingerprint(&patch);
        let mut hunks = HashMap::new();
        let mut media = vec![];
        let ws_dir = ws.dir();
        let mut uncommitted = HashSet::new();
        for repo in &ws.repos {
            match git::uncommitted_files(&ws_dir.join(&repo.name)) {
                Ok(paths) => {
                    uncommitted.extend(paths.into_keys().map(|p| format!("{}/{p}", repo.name)))
                }
                Err(e) => error = Some(format!("{}: {e}", repo.name)),
            }
        }
        let mut parsed: Vec<(DiffFile, u64)> = files::split_patch(&patch)
            .into_iter()
            .filter_map(|chunk| {
                let file = parse_chunk(chunk)?;
                hunks.insert(file.path().clone(), review::parse_hunks(chunk));
                media.extend(media::MediaFile::parse(file.path(), chunk, &ws_dir));
                Some((file, review::chunk_fingerprint(chunk)))
            })
            .collect();
        // A local edit can undo a committed change back to the base, leaving no base patch.
        // Keep its row available for the uncommitted marker and discard action.
        let present: HashSet<_> = parsed.iter().map(|(f, _)| f.path().to_string()).collect();
        for path in uncommitted.difference(&present) {
            parsed.push((DiffFile::unchanged(path.clone(), ""), 0));
        }
        let paths: Vec<String> = parsed.iter().map(|(f, _)| f.path().to_string()).collect();
        let order = files::tree_order(&paths);
        let mut slots: Vec<Option<(DiffFile, u64)>> = parsed.into_iter().map(Some).collect();
        let (files, fps) = order.into_iter().filter_map(|i| slots[i].take()).unzip();
        DiffData {
            bases: ws
                .repos
                .iter()
                .map(|r| (r.name.clone(), r.base.clone()))
                .collect(),
            revs: (ws.repos.iter())
                .map(|r| {
                    let rev = review::compare_rev(&ws_dir.join(&r.name), &r.base, uncommitted_only);
                    (r.name.clone(), rev)
                })
                .collect(),
            fp,
            files,
            fps,
            error,
            uncommitted,
            hunks,
            media,
            edited: None,
        }
    }
}

impl Differ {
    pub(crate) fn refresh_diff(&mut self, cx: &mut Context<Self>) {
        let Some(ws) = self.active_ws().cloned() else {
            return;
        };
        self.diff_gen += 1;
        let generation = self.diff_gen;
        let uncommitted_only = self.preferences.uncommitted_only;
        let edited = self.editing.as_ref().map(|e| e.abs.clone());
        cx.spawn(async move |this, cx| {
            let name = ws.name.clone();
            let data = cx
                .background_executor()
                .spawn(async move {
                    let edited = edited.map(|abs| (abs.clone(), std::fs::read(abs).ok()));
                    DiffData {
                        edited,
                        ..DiffData::load(&ws, uncommitted_only)
                    }
                })
                .await;
            this.update(cx, |s, cx| s.complete_refresh(&name, generation, data, cx))
                .ok();
        })
        .detach();
    }

    pub(crate) fn complete_refresh(
        &mut self,
        name: &str,
        generation: u64,
        data: DiffData,
        cx: &mut Context<Self>,
    ) {
        if self.workspace_name() == Some(name) && self.diff_gen == generation {
            self.apply_diff(data, cx);
        }
    }

    pub(crate) fn apply_diff(&mut self, mut data: DiffData, cx: &mut Context<Self>) {
        if let Some((abs, bytes)) = data.edited.take() {
            self.edit_disk(&abs, bytes, cx);
        }
        // The buffer stays intact when a PR is retargeted or its base ref moves.
        if let (Some(ws), Some(e)) = (self.active_ws().cloned(), &mut self.editing) {
            let error = if let Some(error) = &data.error {
                Some(error.clone())
            } else if data.fp != self.diff_fp {
                let file = data
                    .files
                    .iter()
                    .find(|f| *f.path() == e.path)
                    .cloned()
                    .or_else(|| {
                        (e.dirty || e.saved).then(|| DiffFile::unchanged(e.path.clone(), ""))
                    });
                file.and_then(|f| e.rebase(&ws, &f).err())
            } else {
                None
            };
            if let Some(error) = error {
                self.diff_error = Some(error);
                cx.notify();
                return;
            }
        }
        let status_changed = !self.diff_loaded
            || data.bases != self.diff_bases
            || data.revs != self.diff_revs
            || data.error != self.diff_error
            || data.uncommitted != self.uncommitted;
        self.diff_bases = data.bases;
        self.diff_revs = data.revs;
        self.diff_loaded = true;
        self.diff_error = data.error;
        self.uncommitted = data.uncommitted;
        // Turn ends and saves refresh often; an unchanged patch keeps scroll and expansion.
        if data.fp == self.diff_fp
            && !self.diff_files.is_empty()
            && self
                .diff_files
                .iter()
                .map(|f| f.path())
                .eq(data.files.iter().map(|f| f.path()))
        {
            if status_changed {
                cx.notify();
            }
            return;
        }
        // The edited file left the diff (say `git checkout -- f`): clean, editing ends; unsaved
        // edits keep it in the diff.
        if let Some(e) = &self.editing
            && !data.files.iter().any(|f| *f.path() == e.path)
        {
            // Saved back to the base by this edit: still editing.
            match e.dirty || e.saved {
                // git's last view of it, so `orig` stays git's and not the buffer.
                // In tree order, so the same files stay in place.
                true => {
                    let paths: Vec<String> = (data.files.iter().map(|f| f.path().to_string()))
                        .chain([e.path.to_string()])
                        .collect();
                    let order = files::tree_order(&paths);
                    let at = order
                        .iter()
                        .position(|&i| i == paths.len() - 1)
                        .unwrap_or(0);
                    data.files.insert(at, e.orig.0.clone());
                    data.hunks.insert(e.path.clone(), e.orig.1.clone());
                    data.fps.insert(at, 0);
                }
                false => self.end_edit(cx),
            }
        }
        // The file edited in place shows its buffer, saved or not; git's comes back after.
        if let Some(e) = &mut self.editing
            && let Some(f) = data.files.iter_mut().find(|f| *f.path() == e.path)
        {
            let hunks = data
                .hunks
                .insert(e.path.clone(), review::parse_hunks(&e.patch));
            e.orig = (
                std::mem::replace(f, e.file.clone()),
                hunks.unwrap_or_default(),
            );
        }
        // With the same files, only those that changed are replaced, so the others keep their
        // folds, annotations and selection and the view stays put.
        let in_place = !self.diff_files.is_empty()
            && (self.diff_files.iter().map(|f| f.path())).eq(data.files.iter().map(|f| f.path()));
        let old_fps = std::mem::take(&mut self.diff_fps);
        self.review.hunks = data.hunks;
        self.set_media(data.media, cx);
        if let Some(ws) = self.workspace_name().map(str::to_string) {
            self.reanchor_comments(&ws);
        }
        let was_viewed = self.viewed_now.clone();
        // set_files scrolls back to the top; come back to the file the user was on.
        let kept = self
            .diff_files
            .get(self.diff_file)
            .map(|f| f.path().clone());
        let tree_on_file = (self.tree_rows.get(self.tree_sel)).is_none_or(|r| r.file.is_some());
        // First files of this workspace's diff: no mouse scroll yet, so `j` starts a line cursor.
        let first_load = self.diff_files.is_empty();
        self.diff_fp = data.fp;
        self.diff_fps = data.fps;
        self.diff_files = data.files;
        self.diff_file = kept
            .and_then(|p| self.diff_files.iter().position(|f| *f.path() == p))
            .unwrap_or(self.diff_file.min(self.diff_files.len().saturating_sub(1)));
        self.rebuild_tree();
        if tree_on_file {
            self.follow_tree(self.diff_file);
        }
        self.recompute_viewed();
        let files = self.diff_files.clone();
        let viewed = self.viewed_now.clone();
        let at = self.diff_file;
        let fps = self.diff_fps.clone();
        // A collapsed card would take the line input away.
        let edited = self.editing.as_ref().map(|e| e.path.clone());
        self.diff.update(cx, |d, cx| {
            match in_place {
                true => (files.iter().zip(fps.iter().zip(&old_fps)))
                    .filter(|(_, (new, old))| new != old)
                    .for_each(|(f, _)| d.replace_file(f.clone(), cx)),
                false => d.set_files(files.iter().cloned(), cx),
            }
            for f in &files {
                let path = f.path();
                if viewed.contains(path) && edited.as_ref() != Some(path) {
                    d.set_file_collapsed(path, true, cx);
                } else if was_viewed.contains(path) {
                    d.set_file_collapsed(path, false, cx);
                }
            }
        });
        if first_load && let Some(f) = files.get(at) {
            self.review.at_file(f.path());
        }
        // Editing: the caret's line, so its input is drawn and keeps focus. Else the keyboard
        // view comes back to its line, or the file last moved to.
        let caret = (self.editing.as_ref()).map(|e| e.caret());
        match caret {
            _ if in_place => {}
            Some(caret) => self.diff.update(cx, |d, cx| d.scroll_to_line(caret, cx)),
            None if self.restore_view(cx) => {}
            None => {
                if let Some(f) = files.get(at) {
                    self.diff.update(cx, |d, cx| d.scroll_to_file(f.path(), cx));
                }
            }
        }
        cx.notify();
    }

    /// Keeps the row (and any playing video) of a media file whose blobs didn't change.
    fn set_media(&mut self, files: Vec<media::MediaFile>, cx: &mut Context<Self>) {
        let mut old = std::mem::take(&mut self.media);
        for file in files {
            let path = file.path.clone();
            let view = match old.iter().position(|(_, v)| v.read(cx).file == file) {
                Some(at) => old.swap_remove(at).1,
                None => {
                    let (cache, width) = (self.media_cache.clone(), self.media_width.clone());
                    let view = cx.new(|_| media::MediaView::new(file, cache, width));
                    // One video plays at a time.
                    cx.subscribe(&view, |this, playing, _: &media::Playing, cx| {
                        for (_, v) in &this.media {
                            if v != &playing {
                                v.update(cx, |v, cx| v.stop(cx));
                            }
                        }
                    })
                    .detach();
                    view
                }
            };
            self.media.push((path, view));
        }
    }

    /// `p`: play or pause the video of the file last moved to.
    pub(crate) fn play_media(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(path) =
            (self.cursor_file(window, cx)).map(|ix| self.diff_files[ix].path().clone())
        else {
            return;
        };
        if let Some((_, v)) = self.media.iter().find(|(p, _)| *p == path) {
            v.update(cx, |v, cx| {
                if v.file.kind == media::Kind::Video {
                    v.toggle_default(cx)
                }
            });
        }
    }

    pub(crate) fn viewed(&self, ix: usize) -> Viewed {
        let (Some(ws), Some(f)) = (self.active_ws(), self.diff_files.get(ix)) else {
            return Viewed::No;
        };
        match ws.viewed.get(f.path().as_str()) {
            None => Viewed::No,
            Some(fp) if *fp == self.diff_fps[ix] => Viewed::Yes,
            Some(_) => Viewed::Changed,
        }
    }

    fn recompute_viewed(&mut self) {
        let set = (0..self.diff_files.len())
            .filter(|&ix| self.viewed(ix) == Viewed::Yes)
            .map(|ix| self.diff_files[ix].path().clone())
            .collect();
        self.viewed_now = Rc::new(set);
    }

    pub(crate) fn toggle_viewed(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.diff_files.len() {
            self.set_viewed(&[ix], self.viewed(ix) != Viewed::Yes, cx);
        }
    }

    /// Files under folder `dir` of the tree that the filter shows.
    fn dir_files(&self, dir: &str) -> Vec<usize> {
        let prefix = format!("{dir}/");
        (0..self.diff_files.len())
            .filter(|&ix| self.diff_files[ix].path().starts_with(&prefix) && self.tree_match(ix))
            .collect()
    }

    /// File `ix` matches the "Filter files…" query.
    fn tree_match(&self, ix: usize) -> bool {
        (self.diff_files[ix].path().to_lowercase()).contains(&self.tree_query)
    }

    /// A folder's box: ticks every file inside, or unticks them all when all are viewed.
    pub(crate) fn toggle_viewed_dir(&mut self, dir: &str, cx: &mut Context<Self>) {
        let files = self.dir_files(dir);
        let on = !files.iter().all(|&ix| self.viewed(ix) == Viewed::Yes);
        self.set_viewed(&files, on, cx);
    }

    fn set_viewed(&mut self, files: &[usize], on: bool, cx: &mut Context<Self>) {
        let Some(name) = self.workspace_name().map(str::to_string) else {
            return;
        };
        let Some(ws) = self.workspace.as_mut().filter(|w| w.name == name) else {
            return;
        };
        for &ix in files {
            let path = self.diff_files[ix].path().to_string();
            match on {
                true => ws.viewed.insert(path, self.diff_fps[ix]),
                false => ws.viewed.remove(&path),
            };
        }
        // Files outside the current diff (say in Uncommitted mode) keep their marks.
        cx.emit(DifferEvent::Viewed {
            workspace: name,
            viewed: ws.viewed.clone(),
        });
        self.recompute_viewed();
        // The file being edited stays open, or its line input would go.
        let edited = self.editing.as_ref().map(|e| e.path.clone());
        let paths: Vec<SharedString> = files
            .iter()
            .map(|&ix| self.diff_files[ix].path().clone())
            .filter(|p| !on || Some(p) != edited.as_ref())
            .collect();
        // ponytail: the diff rebuilds once per file; add a batch setter to gpui-kit if big folders lag.
        self.diff.update(cx, |d, cx| {
            paths.iter().for_each(|p| d.set_file_collapsed(p, on, cx))
        });
        cx.notify();
    }

    pub(crate) fn toggle_viewed_path(&mut self, path: &str, cx: &mut Context<Self>) {
        if let Some(ix) = self.diff_files.iter().position(|f| f.path() == path) {
            self.toggle_viewed(ix, cx);
        }
    }

    pub(crate) fn rebuild_tree(&mut self) {
        let keep: Vec<usize> = (0..self.diff_files.len())
            .filter(|&ix| self.tree_match(ix))
            .collect();
        let paths: Vec<String> = (keep.iter())
            .map(|&ix| self.diff_files[ix].path().to_string())
            .collect();
        // A filter shows every match, inside folded folders too.
        let none = HashSet::new();
        let collapsed = match self.tree_query.is_empty() {
            true => &self.collapsed_dirs,
            false => &none,
        };
        self.tree_rows = files::rows(&paths, collapsed);
        // Rows keep the file's index in the whole diff.
        for r in &mut self.tree_rows {
            r.file = r.file.map(|i| keep[i]);
        }
        self.tree_sel = self.tree_sel.min(self.tree_rows.len().saturating_sub(1));
    }

    /// Typing in "Filter files…": the cursor goes to the first match.
    pub(crate) fn filter_tree(&mut self, query: String, cx: &mut Context<Self>) {
        self.tree_query = query.trim().to_lowercase();
        self.rebuild_tree();
        if let Some(row) = self.tree_rows.iter().position(|r| r.file.is_some()) {
            self.tree_sel = row;
        }
        cx.notify();
    }

    /// Enter in the filter: show the match under the cursor.
    pub(crate) fn filter_jump(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if (self.tree_rows.get(self.tree_sel)).is_some_and(|r| r.file.is_some()) {
            self.tree_confirm(self.tree_sel, window, cx);
            self.focus_diff(window, cx);
        }
    }

    /// `s` and the Split / Unified button; the choice persists.
    pub(crate) fn toggle_diff_mode(&mut self, cx: &mut Context<Self>) {
        self.preferences.unified = !self.preferences.unified;
        cx.emit(DifferEvent::Preference(Preference::Unified(
            self.preferences.unified,
        )));
        let mode = diff_mode(self.preferences.unified);
        self.diff.update(cx, |d, cx| d.set_mode(mode, cx));
        cx.notify();
    }

    pub(crate) fn toggle_uncommitted(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.request_discard(AfterDiscard::ToggleUncommitted, window, cx);
    }

    /// Trailing controls of a file card's header, after its "+a −d": the review chip and
    /// Comment button, then the edit buttons; only [Review] [Save ⌘S] while it is edited.
    fn card_header_suffix(&self, file: &DiffFile, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let dirty = (self.editing.as_ref())
            .filter(|e| e.path == *file.path())
            .map(|e| e.dirty);
        (dirty.is_none())
            .then(|| self.review_chips(file.path(), cx))
            .into_iter()
            .chain([inline_edit::edit_buttons(
                file,
                dirty,
                cx.entity().downgrade(),
            )])
            .collect()
    }

    /// Files belongs only to the active Diff artifact.
    pub(crate) fn tree_shown(&self) -> bool {
        self.show_tree && self.workspace.is_some()
    }

    /// The Diff artifact's internal split; the outer Features rail never resizes.
    pub(crate) fn with_tree(
        &self,
        main: impl IntoElement,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let shown = self.tree_shown();
        let width = px(self.preferences.tree_width.unwrap_or(244.));
        v_flex()
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                h_resizable("files-panel")
                    .with_state(&self.tree_resize)
                    .with_handle_appearance(super::quiet_edge())
                    .child(resizable_panel().min_w_0().child(main))
                    .child(
                        resizable_panel()
                            .visible(shown)
                            .size(width)
                            .size_range(px(200.)..px(560.))
                            .fixed()
                            .when(shown, |p| p.child(self.render_tree(window, cx))),
                    ),
            )
            .children(self.render_sheet(window, cx))
    }

    /// Puts the tree cursor on file `ix`'s row, when it has one (not inside a folded folder).
    pub(crate) fn follow_tree(&mut self, ix: usize) -> Option<usize> {
        let row = self.tree_rows.iter().position(|r| r.file == Some(ix))?;
        self.tree_sel = row;
        Some(row)
    }

    pub(crate) fn tree_select(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(r) = self.tree_rows.get(row) else {
            return;
        };
        self.tree_sel = row;
        self.tree_scroll
            .scroll_to_item(row, ScrollStrategy::Nearest);
        if let Some(ix) = r.file {
            self.diff_file = ix;
            {
                let path = self.diff_files[ix].path().to_string();
                self.review.at_file(&path);
                // A line cursor left in the previous file would win over this one for `c`/`i`.
                self.diff.update(cx, |d, cx| {
                    d.set_selected_lines(None, cx);
                    d.scroll_to_file(&path, cx)
                });
            }
        }
        cx.notify();
    }

    pub(crate) fn set_dir_open(&mut self, open: Option<bool>, cx: &mut Context<Self>) {
        let Some(r) = self.tree_rows.get(self.tree_sel) else {
            return;
        };
        if r.file.is_some() {
            // `h` on a file jumps to its folder, like most tree views.
            if open == Some(false)
                && let Some(up) = (0..self.tree_sel)
                    .rev()
                    .find(|&i| self.tree_rows[i].depth < r.depth)
            {
                self.tree_select(up, cx);
            }
            return;
        }
        let path = r.path.clone();
        let is_open = !self.collapsed_dirs.contains(&path);
        // A filter shows every folder open; folding would change hidden state only.
        if open.unwrap_or(!is_open) == is_open || !self.tree_query.is_empty() {
            return;
        }
        match is_open {
            true => self.collapsed_dirs.insert(path),
            false => self.collapsed_dirs.remove(&path),
        };
        self.rebuild_tree();
        cx.notify();
    }

    /// Enter / click on a tree row: toggle a folder or show its file in Diff.
    fn tree_confirm(&mut self, row: usize, _window: &mut Window, cx: &mut Context<Self>) {
        self.tree_select(row, cx);
        if self.tree_rows.get(row).is_some_and(|r| r.file.is_none()) {
            self.set_dir_open(None, cx);
        }
    }

    /// The file a row action applies to: the tree cursor in the tree, the open file in the file
    /// view, else the file last moved to in the diff.
    pub(crate) fn cursor_file(&self, window: &Window, _cx: &App) -> Option<usize> {
        if self.tree_shown() && self.tree_focus.is_focused(window) {
            return self.tree_rows.get(self.tree_sel)?.file;
        }
        // The diff can't say which file is on screen; use the last one moved to, which the
        // tree cursor follows.
        (self.diff_file < self.diff_files.len()).then_some(self.diff_file)
    }

    /// Loading, git failed, or nothing changed; `None` when there is a diff to show.
    pub(crate) fn diff_placeholder(&self, cx: &App) -> Option<AnyElement> {
        let theme = cx.theme();
        let big = |name, color| Icon::new(name).with_size(px(32.)).text_color(color);
        let (icon, title, detail) = match &self.diff_error {
            Some(e) => (
                big(IconName::TriangleAlert, theme.warning_foreground).into_any_element(),
                "Could not load the diff",
                Some(e.clone()),
            ),
            None if !self.diff_loaded => (
                Spinner::new()
                    .with_size(px(32.))
                    .color(faint(theme))
                    .into_any_element(),
                "Loading diff…",
                None,
            ),
            None if self.diff_files.is_empty() => (
                big(IconName::FileDiff, faint(theme)).into_any_element(),
                if self.preferences.uncommitted_only {
                    "No uncommitted changes"
                } else {
                    "No changes"
                },
                None,
            ),
            None => return None,
        };
        Some(
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .px_4()
                .text_color(theme.muted_foreground)
                .child(icon)
                .child(div().text_base().text_color(theme.foreground).child(title))
                .when_some(detail, |d, e| {
                    d.child(div().max_w(px(560.)).text_sm().text_center().child(e))
                })
                .child(h_flex().gap_2().text_sm().child(kbd("r")).child("refresh"))
                .into_any_element(),
        )
    }

    pub(crate) fn render_diff(&self, _window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let viewed = self.viewed_now.clone();
        let shark = cx.entity().downgrade();
        let placeholder = self.diff_placeholder(cx);
        let empty = placeholder.is_some();
        let (size, row) = crate::editor::diff_font(cx);
        let diff_with_annotations =
            self.with_comments(Diff::new(&self.diff).code_font(size, row), cx);
        let diff_with_annotations = (self.with_edit_parts(diff_with_annotations, cx))
            .hunk_separator(DiffHunkSeparator::LineInfo)
            .flex_1()
            .min_h_0();
        let status = self.edit_status();
        // The letter keys' bindings are `DiffPane && !Input`: typing in the line input or the
        // composer never reaches them.
        let pane = v_flex()
            .key_context("DiffPane")
            .relative()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(self.viewport_probe())
            .on_scroll_wheel(cx.listener(|this, _, _, _| this.lose_view()))
            // The diff's own scrolling keys move it without the line cursor.
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, w, cx| {
                let k = ev.keystroke.key.as_str();
                let typing = (this.editing.as_ref()).is_some_and(|e| e.focused(w, cx));
                if !typing && matches!(k, "up" | "down" | "pageup" | "pagedown" | "home" | "end") {
                    this.lose_view();
                }
            }))
            .on_action(cx.listener(|this, _: &SelectNext, w, cx| this.diff_move(1, None, w, cx)))
            .on_action(cx.listener(|this, _: &SelectPrev, w, cx| this.diff_move(-1, None, w, cx)))
            .on_action(cx.listener(|this, _: &PageDown, w, cx| this.diff_move(2, None, w, cx)))
            .on_action(cx.listener(|this, _: &PageUp, w, cx| this.diff_move(-2, None, w, cx)))
            .on_action(
                cx.listener(|this, _: &SelectFirst, w, cx| this.diff_move(0, Some(true), w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectLast, w, cx| this.diff_move(0, Some(false), w, cx)),
            )
            .on_action(cx.listener(|this, _: &AddComment, w, cx| this.comment_dialog(w, cx)))
            .on_action(
                cx.listener(|this, _: &CancelEdit, w, cx| match this.editing {
                    Some(_) => this.leave_edit(w, cx),
                    None => cx.propagate(),
                }),
            )
            .on_action(cx.listener(|this, _: &SendComments, w, cx| this.open_sheet(w, cx)))
            .on_action(
                cx.listener(|this, _: &EditThread, w, cx| this.at_cursor(Self::edit_thread, w, cx)),
            )
            .on_action(cx.listener(|this, _: &DeleteThread, w, cx| {
                this.at_cursor(Self::delete_thread, w, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ViewChange, w, cx| this.at_cursor(Self::view_change, w, cx)),
            )
            .on_action(cx.listener(|this, _: &ResolveThread, _, cx| this.resolve_thread(cx)))
            .on_action(cx.listener(|this, _: &ReopenThread, w, cx| this.reopen_thread(w, cx)))
            .on_action(cx.listener(|this, _: &NextThread, w, cx| this.thread_jump(true, w, cx)))
            .on_action(cx.listener(|this, _: &PrevThread, w, cx| this.thread_jump(false, w, cx)))
            .on_action(cx.listener(|this, _: &ResolveAddressed, _, cx| this.resolve_addressed(cx)))
            .on_action(cx.listener(|this, _: &RequeueOutdated, _, cx| this.requeue_outdated(cx)))
            .on_action(cx.listener(|this, _: &NextHunk, w, cx| this.diff_change(true, w, cx)))
            .on_action(cx.listener(|this, _: &PrevHunk, w, cx| this.diff_change(false, w, cx)))
            .on_action(cx.listener(|this, _: &NextFile, _, cx| this.step_file(1, cx)))
            .on_action(cx.listener(|this, _: &PrevFile, _, cx| this.step_file(-1, cx)))
            .on_action(cx.listener(|this, _: &RefreshDiff, _, cx| this.refresh_diff(cx)))
            .on_action(cx.listener(|this, _: &PlayMedia, w, cx| this.play_media(w, cx)))
            .on_action(cx.listener(|this, _: &ToggleDiffMode, _, cx| this.toggle_diff_mode(cx)))
            .on_action(cx.listener(|this, _: &ToggleViewed, w, cx| {
                if let Some(ix) = this.cursor_file(w, cx) {
                    this.toggle_viewed(ix, cx)
                }
            }))
            // The diff isn't drawn, so its focus handle lands here and the pane's keys still work.
            .when(empty, |d| {
                d.track_focus(&self.diff.read(cx).focus_handle(cx))
            })
            .children(placeholder)
            .when(!empty, |d| {
                d.child(
                    diff_with_annotations
                        .render_header_suffix({
                            let shark = shark.clone();
                            move |file, _, cx| {
                                h_flex().gap(px(10.)).children(
                                    (shark.update(cx, |s, cx| s.card_header_suffix(file, cx)))
                                        .unwrap_or_default(),
                                )
                            }
                        })
                        .render_header_prefix(move |file, _, _| {
                            // Viewed, before the name.
                            let path = file.path().clone();
                            let shark = shark.clone();
                            Checkbox::new(SharedString::from(format!("viewed:{path}")))
                                .small()
                                .tooltip("Viewed (v)")
                                .checked(viewed.contains(&path))
                                .on_click(move |_, _, cx| {
                                    shark
                                        .update(cx, |s, cx| s.toggle_viewed_path(&path, cx))
                                        .ok();
                                })
                        }),
                )
            });
        v_flex()
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_hidden()
            .child(pane)
            .children(self.render_orphans(cx))
            // One bottom bar: in edit mode it says what editing is doing.
            .when(!empty, |d| {
                d.child(self.render_review_bar(status.map(Into::into), cx))
            })
            .into_any_element()
    }

    pub(crate) fn diff_base(&self, repo: &str) -> &str {
        self.diff_bases
            .get(repo)
            .map(String::as_str)
            .or_else(|| {
                self.active_ws()?
                    .repos
                    .iter()
                    .find(|r| r.name == repo)
                    .map(|r| r.base.as_str())
            })
            .unwrap_or("base")
    }

    /// The bases the diff compares against, and its total added and deleted lines.
    pub(crate) fn changes_stats(&self) -> (String, usize, usize) {
        let mut bases: Vec<&str> = (self.active_ws().into_iter())
            .flat_map(|ws| ws.repos.iter().map(|r| self.diff_base(&r.name)))
            .collect();
        bases.sort_unstable();
        bases.dedup();
        let (adds, dels) = (self.diff_files.iter())
            .fold((0, 0), |(a, d), f| (a + f.additions(), d + f.deletions()));
        let base = if self.preferences.uncommitted_only {
            "HEAD".into()
        } else {
            bases.join(", ")
        };
        (base, adds, dels)
    }

    /// Compact controls embedded in the branch/sync header.
    pub(crate) fn render_diff_controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (base, adds, dels) = self.changes_stats();
        let n = self.diff_files.len();
        h_flex()
            .flex_none()
            .gap(px(8.))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, e: &MouseDownEvent, w, cx| {
                    let entries = this.changes_menu(cx);
                    this.show_menu(e.position, entries, w, cx);
                }),
            )
            .child(
                Button::new("uncommitted-only")
                    .xsmall()
                    .h(px(28.))
                    .px(px(10.))
                    .rounded(px(6.))
                    .outline()
                    .selected(self.preferences.uncommitted_only)
                    .toggled(self.preferences.uncommitted_only)
                    .accessibility_label("Uncommitted changes only")
                    .label(if self.preferences.uncommitted_only {
                        "Uncommitted"
                    } else {
                        "All changes"
                    })
                    .tooltip(if self.preferences.uncommitted_only {
                        "HEAD → working tree: staged, unstaged, and untracked changes. Click for all changes: workspace base → working tree.".to_string()
                    } else {
                        format!("Workspace base ({base}) → working tree: committed and uncommitted changes. Click for uncommitted changes: HEAD → working tree.")
                    })
                    .on_click(cx.listener(|this, _, w, cx| this.toggle_uncommitted(w, cx))),
            )
            .child(
                Button::new("diff-mode")
                    .xsmall()
                    .size(px(28.))
                    .px_0()
                    .rounded(px(6.))
                    .icon(if self.preferences.unified {
                        IconName::Rows2
                    } else {
                        IconName::Columns2
                    })
                    .toggled(self.preferences.unified)
                    .accessibility_label("Unified diff")
                    .tooltip(if self.preferences.unified {
                        "Unified view · Click for Split (s)"
                    } else {
                        "Split view · Click for Unified (s)"
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_diff_mode(cx))),
            )
            .child(
                Button::new("files-toggle")
                    .xsmall()
                    .h(px(28.))
                    .rounded(px(6.))
                    .outline()
                    .selected(self.show_tree)
                    .w(px(28.))
                    .px_0()
                    .icon(IconName::PanelRight)
                    .toggled(self.show_tree)
                    .accessibility_label("Files")
                    .tooltip(format!(
                        "{} files (cmd-alt-b) · {n} files · {}/{n} viewed · +{adds} −{dels}",
                        if self.show_tree { "Hide" } else { "Show" },
                        self.viewed_now.len(),
                    ))
                    .on_click(cx.listener(|this, _, w, cx| this.toggle_tree(w, cx))),
            )
    }

    /// Side headings and the line input of the file edited in place; its header buttons come
    /// through `card_header_suffix`.
    fn with_edit_parts(&self, diff: Diff, cx: &mut Context<Self>) -> Diff {
        let shark = cx.entity().downgrade();
        let clicks = shark.clone();
        self.with_inline_edit(diff, cx)
            .accent_working(self.editing.as_ref().map(|e| e.path.clone()))
            .side_headings(move |file, _, cx| {
                let repo = file.path().split('/').next().unwrap_or_default();
                (shark.read_with(cx, |s, _| {
                    let base = if s.preferences.uncommitted_only {
                        "HEAD"
                    } else {
                        s.diff_base(repo)
                    };
                    (
                        format!("Original · {base} · read only").into(),
                        s.working_heading(file.path()).into(),
                    )
                }))
                .unwrap_or_default()
            })
            // A click on a working line of the edited file moves the caret there.
            .on_line_click(move |pos, _, window, cx| {
                clicks
                    .update(cx, |s, cx| s.edit_click(pos, window, cx))
                    .ok();
            })
    }

    pub(crate) fn render_tree(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, fg, hint) = (theme.muted_foreground, theme.foreground, faint(theme));
        let focused = self.tree_focus.is_focused(window);
        let n = self.diff_files.len();
        let tree = v_flex()
            .id("file-tree")
            .test_support()
            .key_context("FileTree")
            .track_focus(&self.tree_focus)
            .flex_1()
            .min_h_0()
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                let to = (this.tree_sel + 1).min(this.tree_rows.len().saturating_sub(1));
                this.tree_select(to, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| {
                this.tree_select(this.tree_sel.saturating_sub(1), cx)
            }))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| this.tree_select(0, cx)))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| {
                this.tree_select(this.tree_rows.len().saturating_sub(1), cx)
            }))
            .on_action(cx.listener(|this, _: &PageDown, _, cx| {
                let to = (this.tree_sel + 15).min(this.tree_rows.len().saturating_sub(1));
                this.tree_select(to, cx)
            }))
            .on_action(cx.listener(|this, _: &PageUp, _, cx| {
                this.tree_select(this.tree_sel.saturating_sub(15), cx)
            }))
            .on_action(
                cx.listener(|this, _: &Confirm, w, cx| this.tree_confirm(this.tree_sel, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &CollapseDir, _, cx| this.set_dir_open(Some(false), cx)),
            )
            .on_action(cx.listener(|this, _: &ExpandDir, _, cx| this.set_dir_open(Some(true), cx)))
            .on_action(cx.listener(|this, _: &ToggleViewed, _, cx| {
                let Some(r) = this.tree_rows.get(this.tree_sel) else {
                    return;
                };
                match r.file {
                    Some(ix) => this.toggle_viewed(ix, cx),
                    None => this.toggle_viewed_dir(&r.path.clone(), cx),
                }
            }))
            .child(
                uniform_list(
                    "file-tree-rows",
                    self.tree_rows.len(),
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|ix| this.render_tree_row(ix, focused, cx))
                            .collect()
                    }),
                )
                .track_scroll(&self.tree_scroll)
                .flex_1(),
            );
        // The filter sits outside `FileTree`, whose letter keys would eat its typing.
        v_flex()
            .size_full()
            .px(px(8.))
            .border_l_1()
            .border_color(theme.border)
            .text_color(fg)
            .child(
                h_flex()
                    .flex_none()
                    .px(px(8.))
                    .pt_4()
                    .pb_1()
                    .text_xs()
                    .text_color(muted)
                    .child(side_header(theme, "Files").p_0().flex_1())
                    .child(format!("{}/{n} viewed", self.viewed_now.len())),
            )
            .child(
                Input::new(&self.tree_filter)
                    .h(px(32.))
                    .rounded(theme.radius)
                    .cleanable(true)
                    .prefix(
                        Icon::new(IconName::Search)
                            .with_size(px(14.))
                            .text_color(muted),
                    ),
            )
            .child(tree)
            .child(
                div()
                    .p(px(12.))
                    .text_xs()
                    .text_color(hint)
                    .child("Enter jump to diff · v viewed"),
            )
    }

    fn render_tree_row(&self, ix: usize, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let r = &self.tree_rows[ix];
        let selected = ix == self.tree_sel;
        let open_file = self.editor_path.clone();
        let depth = r.depth;
        let next_depth = self.tree_rows.get(ix + 1).map_or(0, |r| r.depth);
        let guide = theme.border;
        let folder_color = theme.foreground;
        // `uniform_list` rows: no margin, so the 30px height is the pitch.
        let row = side_row(theme)
            .id(("tree-row", ix))
            .test_support()
            .w_full()
            .mx_0()
            .mb_0()
            .pr_2()
            .pl(px(7. + 12. * r.depth as f32))
            .map(|d| list_cursor_row(d, selected, focused, theme))
            .on_click(cx.listener(move |this, e: &ClickEvent, w, cx| {
                this.tree_focus.focus(w, cx);
                match (
                    e.modifiers().platform,
                    e.click_count(),
                    this.tree_rows.get(ix).and_then(|r| r.file),
                ) {
                    (true, _, Some(file)) => this.open_file_tab(file, w, cx),
                    (_, 2, Some(file)) => this.open_file(file, w, cx),
                    _ => this.tree_confirm(ix, w, cx),
                }
            }))
            // Right-click moves the cursor too, so it's clear which row the menu is for.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &MouseDownEvent, w, cx| {
                    this.tree_select(ix, cx);
                    let Some(r) = this.tree_rows.get(ix) else {
                        return;
                    };
                    let entries = match r.file {
                        Some(f) => this.file_menu(&this.diff_files[f].path().clone(), cx),
                        None => this.dir_menu(&r.path.clone(), cx),
                    };
                    this.show_menu(e.position, entries, w, cx);
                }),
            )
            .child(
                // Paint only: no hitbox, and cover the row borders to keep virtualized segments joined.
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        for ancestor in 0..depth {
                            let height: f32 = if next_depth > ancestor { 30. } else { 15. };
                            let top = if selected && focused { 1. } else { 0. };
                            let bottom = if selected && focused {
                                height.min(29.)
                            } else {
                                height
                            };
                            window.paint_quad(fill(
                                Bounds::new(
                                    bounds.origin + point(px(16. + 12. * ancestor as f32), px(top)),
                                    size(px(1.), px(bottom - top)),
                                ),
                                guide,
                            ));
                        }
                    },
                )
                .absolute()
                .top(px(-1.))
                .left(px(-1.))
                .w_full()
                .h(px(30.)),
            );
        let Some(file) = r.file else {
            let open = !self.collapsed_dirs.contains(&r.path);
            return row
                // A 14px chevron in a 16px slot, so folder and file names line up.
                .child(
                    h_flex().size(px(16.)).flex_none().justify_center().child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .with_size(px(14.))
                        .text_color(folder_color),
                    ),
                )
                .child(
                    div()
                        .id(("tree-label", ix))
                        .test_support()
                        .flex_1()
                        .truncate()
                        .text_color(folder_color)
                        .child(r.name.clone()),
                )
                .child(self.dir_viewed_box(ix, &r.path, cx))
                .into_any_element();
        };
        let f = &self.diff_files[file];
        let state = self.viewed(file);
        let icon = match f.status() {
            DiffFileStatus::Added => IconName::FilePlus,
            DiffFileStatus::Deleted => IconName::FileMinus,
            _ => IconName::FileDiff,
        };
        let is_open = open_file.as_deref() == Some(f.path().as_str());
        let row = row
            .child(
                Icon::new(icon)
                    .with_size(px(16.))
                    .text_color(theme.muted_foreground),
            )
            .child(
                div()
                    .id(("tree-label", ix))
                    .test_support()
                    .flex_1()
                    .truncate()
                    .text_color(match state {
                        Viewed::Yes => theme.muted_foreground,
                        _ => theme.foreground,
                    })
                    .when(is_open, |d| d.font_weight(FontWeight::MEDIUM))
                    .child(
                        StyledText::new(r.name.clone()).with_highlights(r.name.rfind('/').map(
                            |slash| {
                                (
                                    0..slash + 1,
                                    HighlightStyle {
                                        color: Some(theme.foreground),
                                        ..Default::default()
                                    },
                                )
                            },
                        )),
                    ),
            )
            .when(self.uncommitted.contains(f.path().as_str()), |d| {
                d.child(
                    div()
                        .id(("uncommitted", ix))
                        .test_support()
                        .size(px(6.))
                        .flex_none()
                        .rounded_full()
                        .bg(theme.primary)
                        .aria_label("Uncommitted changes")
                        .tooltip(|w, cx| Tooltip::new("Uncommitted changes").build(w, cx)),
                )
            })
            .when(state == Viewed::Changed, |d| {
                d.child(
                    div()
                        .id(("changed", ix))
                        .size(px(6.))
                        .rounded_full()
                        .bg(theme.warning_foreground)
                        .tooltip(|w, cx| Tooltip::new("Changed since viewed").build(w, cx)),
                )
            })
            // Fixed right-aligned slots, so the counts line up down the tree.
            .child(
                h_flex()
                    .gap_1()
                    .text_xs()
                    .child(
                        h_flex()
                            .min_w(px(20.))
                            .justify_end()
                            .text_color(theme.success_foreground)
                            .when(f.additions() > 0, |d| {
                                d.child(format!("+{}", f.additions()))
                            }),
                    )
                    .child(
                        h_flex()
                            .min_w(px(20.))
                            .justify_end()
                            .text_color(theme.danger_foreground)
                            .when(f.deletions() > 0, |d| {
                                d.child(format!("−{}", f.deletions()))
                            }),
                    ),
            )
            .child(
                div()
                    .id(("viewed-box", ix))
                    // The row's click selects; this one only toggles.
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        Checkbox::new(("viewed", ix))
                            .small()
                            .checked(state == Viewed::Yes)
                            .tooltip("Viewed (v)")
                            .on_click(cx.listener(move |this, _: &bool, _, cx| {
                                this.toggle_viewed(file, cx)
                            })),
                    ),
            );
        row.into_any_element()
    }

    fn dir_viewed_box(&self, ix: usize, dir: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let files = self.dir_files(dir);
        let all = files.iter().all(|&f| self.viewed(f) == Viewed::Yes);
        let dir = dir.to_string();
        div()
            .id(("dir-viewed-box", ix))
            // The row's click folds the folder; this one only toggles.
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                Checkbox::new(("dir-viewed", ix))
                    .small()
                    .checked(all)
                    .tooltip("All viewed (v)")
                    .on_click(
                        cx.listener(move |this, _: &bool, _, cx| this.toggle_viewed_dir(&dir, cx)),
                    ),
            )
    }

    /// Right-click menu of file `path` (`repo/…`): the tree's file rows, the editor header
    /// and the File tab. By path, since the open file need not be in the diff.
    pub(crate) fn file_menu(&self, path: &str, cx: &Context<Self>) -> Vec<Entry> {
        let Some(ws) = self.active_ws() else {
            return vec![];
        };
        let abs = ws.dir().join(path);
        let ix = self.diff_files.iter().position(|f| f.path() == path);
        // Deleted and binary files show in the diff only, as `open_file` and `edit_buttons` say.
        let on_disk = ix.is_none_or(|ix| {
            let f = &self.diff_files[ix];
            f.status() != DiffFileStatus::Deleted && !f.is_binary()
        });
        let open = self.editor_path.as_deref() == Some(path);
        let p = path.to_string();
        let mut entries = vec![];
        if on_disk && !open {
            let p = p.clone();
            entries.push(menu::item(
                IconName::FileCode,
                "Open in editor",
                on_differ(self.workspace_name(), cx, move |s, _w, cx| {
                    s.request_editor(p.clone(), false, false, cx)
                }),
            ));
        }
        if let Some(ix) = ix {
            if on_disk {
                let p = p.clone();
                entries.push(menu::item(
                    IconName::Pencil,
                    "Edit working copy",
                    on_differ(self.workspace_name(), cx, move |s, w, cx| {
                        s.request_activation(cx);
                        s.edit_working_copy(Some(p.clone().into()), w, cx)
                    }),
                ));
            }
            let (p1, p2) = (p.clone(), p.clone());
            entries.extend([
                menu::item(
                    IconName::MessageSquare,
                    "Comment",
                    on_differ(self.workspace_name(), cx, move |s, w, cx| {
                        s.request_activation(cx);
                        s.comment_in(&p1, w, cx)
                    }),
                ),
                menu::item(
                    IconName::Check,
                    "Viewed",
                    on_differ(self.workspace_name(), cx, move |s, _, cx| {
                        s.toggle_viewed_path(&p2, cx)
                    }),
                )
                .checked(self.viewed(ix) == Viewed::Yes),
            ]);
        }
        if self.uncommitted.contains(path) {
            let (name, path) = (ws.name.clone(), path.to_string());
            entries.push(Entry::Sep);
            entries.push(menu::item(
                IconName::Undo,
                "Discard uncommitted changes…",
                on_differ(self.workspace_name(), cx, move |_s, _w, cx| {
                    cx.emit(DifferEvent::DiscardFile {
                        workspace: name.clone(),
                        path: path.clone(),
                    })
                }),
            ));
        }
        entries.push(Entry::Sep);
        if on_disk {
            let a = abs.clone();
            entries.push(menu::item(
                IconName::FolderOpen,
                "Reveal in Finder",
                move |_, cx| cx.reveal_path(&a),
            ));
            for app in crate::apps::installed() {
                if app.kind != crate::apps::Kind::Folder {
                    continue;
                }
                let a = abs.clone();
                let label = format!("Open in {}", app.name);
                let item = menu::item(
                    crate::ui::app_icon(app),
                    label,
                    on_differ(self.workspace_name(), cx, move |_, _, cx| {
                        cx.emit(DifferEvent::OpenApp {
                            app,
                            path: a.clone(),
                        });
                    }),
                );
                entries.push(item.image(app.icon.clone()));
            }
        }
        entries.extend([
            menu::item(IconName::Copy, "Copy path", move |w, cx| {
                copy(abs.display().to_string(), w, cx)
            }),
            // Relative to the repo, as its own tools and the agent's diffs name it.
            menu::item(IconName::Copy, "Copy relative path", move |w, cx| {
                copy(
                    p.split_once('/').map_or(&*p, |(_, rel)| rel).to_string(),
                    w,
                    cx,
                )
            }),
        ]);
        entries
    }

    /// Right-click menu of folder `dir` in the tree; it is the tree cursor's row.
    fn dir_menu(&self, dir: &str, cx: &Context<Self>) -> Vec<Entry> {
        let Some(ws) = self.active_ws() else {
            return vec![];
        };
        let abs = ws.dir().join(dir);
        let files = self.dir_files(dir);
        let all = files.iter().all(|&f| self.viewed(f) == Viewed::Yes);
        let d = dir.to_string();
        let mut entries = vec![
            menu::item(
                IconName::Check,
                "All viewed",
                on_differ(self.workspace_name(), cx, move |s, _, cx| {
                    s.toggle_viewed_dir(&d, cx)
                }),
            )
            .checked(all),
        ];
        // A filter shows every folder open, and folding does nothing then.
        if self.tree_query.is_empty() {
            let open = !self.collapsed_dirs.contains(dir);
            entries.push(menu::item(
                if open {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronDown
                },
                if open { "Collapse" } else { "Expand" },
                on_differ(self.workspace_name(), cx, move |s, _, cx| {
                    s.set_dir_open(Some(!open), cx)
                }),
            ));
        }
        let a = abs.clone();
        entries.extend([
            Entry::Sep,
            menu::item(IconName::FolderOpen, "Reveal in Finder", move |_, cx| {
                cx.reveal_path(&a)
            }),
            menu::item(IconName::Copy, "Copy path", move |w, cx| {
                copy(abs.display().to_string(), w, cx)
            }),
        ]);
        entries
    }

    /// Right-click menu of the Diff button and its header controls.
    pub(crate) fn changes_menu(&self, cx: &Context<Self>) -> Vec<Entry> {
        let mut entries = vec![
            menu::item(
                IconName::FileDiff,
                "Uncommitted only",
                on_differ(self.workspace_name(), cx, |s, w, cx| {
                    s.toggle_uncommitted(w, cx)
                }),
            )
            .checked(self.preferences.uncommitted_only),
            menu::item(
                IconName::Columns2,
                "Unified",
                on_differ(self.workspace_name(), cx, |s, _, cx| s.toggle_diff_mode(cx)),
            )
            .checked(self.preferences.unified),
            menu::item(
                IconName::RefreshCw,
                "Refresh",
                on_differ(self.workspace_name(), cx, |s, _, cx| s.refresh_diff(cx)),
            ),
            menu::item(
                IconName::PanelRight,
                "Show files",
                on_differ(self.workspace_name(), cx, |s, w, cx| s.toggle_tree(w, cx)),
            )
            .checked(self.show_tree),
        ];
        if (self.ws_comments().iter()).any(|c| c.status == review::Status::Pending) {
            entries.extend([
                Entry::Sep,
                menu::item(
                    IconName::Send,
                    "Send review…",
                    on_differ(self.workspace_name(), cx, |s, w, cx| s.open_sheet(w, cx)),
                ),
            ]);
        }
        entries
    }

    fn step_file(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.diff_files.is_empty() {
            return;
        }
        let n = self.diff_files.len() as isize;
        self.diff_file = (self.diff_file as isize + delta).rem_euclid(n) as usize;
        let path = self.diff_files[self.diff_file].path().to_string();
        self.review.at_file(&path);
        // A line cursor left in the previous file would win over the new one for `i`.
        self.diff.update(cx, |d, cx| {
            d.set_selected_lines(None, cx);
            d.scroll_to_file(&path, cx)
        });
        if let Some(row) = self.follow_tree(self.diff_file) {
            self.tree_scroll
                .scroll_to_item(row, ScrollStrategy::Nearest);
        }
        cx.notify();
    }
}

/// The diff's mode for the persisted `unified` choice.
pub(crate) fn diff_mode(unified: bool) -> DiffMode {
    match unified {
        true => DiffMode::Unified,
        false => DiffMode::Split,
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: it would bring `gpui_kit::test` in over the `#[test]` its macro expands to.
    use super::{DiffData, parse_chunk};
    use crate::{
        diff_view::DiffFile,
        features::differ::{Differ, Preferences},
    };
    use gpui_kit::TestAppContext;
    use std::collections::{HashMap, HashSet};

    #[gpui_kit::test]
    fn a_chunk_that_fails_to_parse_stays_in_the_list_with_its_reason(_: &mut TestAppContext) {
        let chunk = "diff --git \"a/caf\\351.txt\" \"b/caf\\351.txt\"\n--- \"a/caf\\351.txt\"\n+++ \"b/caf\\351.txt\"\n@@ -1 +1 @@\n-a\n+b\n";
        let bad =
            "diff --git a/x.txt b/x.txt\n--- a/x.txt\n+++ b/x.txt\n@@ -1,2 +1,2 @@\n-old\né\n";
        assert!(DiffFile::parse(bad).is_err());
        let file = parse_chunk(bad).unwrap();
        assert_eq!(file.path().as_str(), "x.txt");
        assert!(
            file.text_for_lines(crate::diff_view::DiffSide::Modified, 1, 1)
                .contains("Could not parse")
        );
        // Whatever the parser makes of the quoted non-UTF-8 path, the chunk yields a file.
        assert!(parse_chunk(chunk).is_some());
    }

    fn data(patch: &str, fps: [u64; 2]) -> DiffData {
        let files = DiffFile::parse(patch).unwrap();
        DiffData {
            fp: fps[1],
            bases: HashMap::new(),
            revs: HashMap::new(),
            files,
            fps: fps.to_vec(),
            error: None,
            uncommitted: HashSet::new(),
            hunks: HashMap::new(),
            media: vec![],
            edited: None,
        }
    }

    #[gpui_kit::test]
    fn a_refresh_keeps_the_view_state_of_files_that_did_not_change(cx: &mut TestAppContext) {
        use crate::diff_view::{DiffLineRange, DiffSide};
        cx.update(|cx| {
            gpui_kit::component::init(cx);
            crate::diff_view::init(cx);
        });
        let (differ, cx) = cx.add_window_view(|window, cx| {
            Differ::new(
                Preferences::from(&crate::state::State::default()),
                window,
                cx,
            )
        });
        let patch = |b: &str| {
            format!(
                "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n one\n-two\n+TWO\n\
                 diff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-old\n+{b}\n"
            )
        };
        let select = DiffLineRange::new("a.txt", DiffSide::Modified, 2, 2);
        differ.update(cx, |d, cx| {
            d.apply_diff(data(&patch("one"), [1, 2]), cx);
            d.diff
                .update(cx, |s, cx| s.set_selected_lines(Some(select.clone()), cx));
            // Only b.txt changed.
            d.apply_diff(data(&patch("two"), [1, 3]), cx);
            let (selected, text) = d.diff.read_with(cx, |s, _| {
                (
                    s.selected_lines(),
                    s.files()[1].text_for_lines(DiffSide::Modified, 1, 1),
                )
            });
            assert_eq!(selected, Some(select.clone()));
            assert_eq!(text, "two\n");
        });
    }
}
