//! The Diff tab and the file tree: loading, viewed marks, tree navigation.
use super::*;

/// A changed file against the fingerprint stored when it was marked viewed.
#[derive(PartialEq, Clone, Copy, Debug)]
pub(super) enum Viewed {
    No,
    Yes,
    /// Marked viewed, then the agent or the editor changed it.
    Changed,
}

/// One parsed diff refresh, built off the main thread.
pub(super) struct DiffData {
    /// Fingerprint of the whole patch; equal means nothing to redo.
    fp: u64,
    /// In tree order.
    pub(super) files: Vec<DiffFile>,
    fps: Vec<u64>,
    /// git failed for some repo; the patch is then empty, which must not read as "no changes".
    error: Option<String>,
    hunks: HashMap<SharedString, Vec<review::Hunk>>,
    media: Vec<media::MediaFile>,
}

impl DiffData {
    pub(super) fn load(ws: &Workspace) -> DiffData {
        let (patch, error) = match git::combined_diff(ws, &ws.dir()) {
            Ok(p) => (p, None),
            Err(e) => (String::new(), Some(e)),
        };
        let fp = files::fingerprint(&patch);
        let mut hunks = HashMap::new();
        let mut media = vec![];
        let ws_dir = ws.dir();
        let parsed: Vec<(DiffFile, u64)> = files::split_patch(&patch)
            .into_iter()
            .filter_map(|chunk| {
                let file = DiffFile::parse(chunk).ok()?.into_iter().next()?;
                hunks.insert(file.path().clone(), review::parse_hunks(chunk));
                media.extend(media::MediaFile::parse(file.path(), chunk, &ws_dir));
                Some((file, files::fingerprint(chunk)))
            })
            .collect();
        let paths: Vec<String> = parsed.iter().map(|(f, _)| f.path().to_string()).collect();
        let order = files::tree_order(&paths);
        let mut slots: Vec<Option<(DiffFile, u64)>> = parsed.into_iter().map(Some).collect();
        let (files, fps) = order.into_iter().filter_map(|i| slots[i].take()).unzip();
        DiffData {
            fp,
            files,
            fps,
            error,
            hunks,
            media,
        }
    }
}

impl Shark {
    pub(super) fn refresh_diff(&mut self, cx: &mut Context<Self>) {
        let Some(ws) = self.active_ws().cloned() else {
            return;
        };
        self.diff_gen += 1;
        let generation = self.diff_gen;
        cx.spawn(async move |this, cx| {
            let name = ws.name.clone();
            let data = cx
                .background_executor()
                .spawn(async move { DiffData::load(&ws) })
                .await;
            this.update(cx, |s, cx| {
                if s.active.as_ref() != Some(&name) || s.diff_gen != generation {
                    return; // superseded: another workspace, or a newer refresh
                }
                s.apply_diff(data, cx);
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn apply_diff(&mut self, data: DiffData, cx: &mut Context<Self>) {
        let status_changed = !self.diff_loaded || data.error != self.diff_error;
        self.diff_loaded = true;
        self.diff_error = data.error;
        // Turn ends and saves refresh often; an unchanged patch keeps scroll and expansion.
        if data.fp == self.diff_fp && !self.diff_files.is_empty() {
            if status_changed {
                cx.notify();
            }
            return;
        }
        self.review.hunks = data.hunks;
        self.set_media(data.media, cx);
        if let Some(ws) = self.active.clone() {
            self.review.load_comments(&ws);
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
        self.diff.update(cx, |d, cx| {
            d.set_files(files.iter().cloned(), cx);
            for f in &files {
                let path = f.path();
                if viewed.contains(path) {
                    d.set_file_collapsed(path, true, cx);
                } else if was_viewed.contains(path) {
                    d.set_file_collapsed(path, false, cx);
                }
            }
        });
        if first_load && let Some(f) = files.get(at) {
            self.review.at_file(f.path());
        }
        // The keyboard view comes back to its line; otherwise the file last moved to.
        if !self.restore_view(cx)
            && let Some(f) = files.get(at)
        {
            self.diff.update(cx, |d, cx| d.scroll_to_file(f.path(), cx));
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
    pub(super) fn play_media(&mut self, window: &Window, cx: &mut Context<Self>) {
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

    pub(super) fn viewed(&self, ix: usize) -> Viewed {
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

    pub(super) fn toggle_viewed(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.diff_files.len() {
            self.set_viewed(&[ix], self.viewed(ix) != Viewed::Yes, cx);
        }
    }

    /// Files under folder `dir` of the tree.
    fn dir_files(&self, dir: &str) -> Vec<usize> {
        let prefix = format!("{dir}/");
        (0..self.diff_files.len())
            .filter(|&ix| self.diff_files[ix].path().starts_with(&prefix))
            .collect()
    }

    /// A folder's box: ticks every file inside, or unticks them all when all are viewed.
    fn toggle_viewed_dir(&mut self, dir: &str, cx: &mut Context<Self>) {
        let files = self.dir_files(dir);
        let on = !files.iter().all(|&ix| self.viewed(ix) == Viewed::Yes);
        self.set_viewed(&files, on, cx);
    }

    fn set_viewed(&mut self, files: &[usize], on: bool, cx: &mut Context<Self>) {
        let Some(name) = self.active.clone() else {
            return;
        };
        let Some(ws) = self.state.workspaces.iter_mut().find(|w| w.name == name) else {
            return;
        };
        for &ix in files {
            let path = self.diff_files[ix].path().to_string();
            match on {
                true => ws.viewed.insert(path, self.diff_fps[ix]),
                false => ws.viewed.remove(&path),
            };
        }
        // Forget files that left the diff so the map doesn't grow forever.
        let live: HashSet<&str> = self.diff_files.iter().map(|f| f.path().as_str()).collect();
        ws.viewed.retain(|p, _| live.contains(p.as_str()));
        self.state.save();
        self.recompute_viewed();
        let paths: Vec<SharedString> = files
            .iter()
            .map(|&ix| self.diff_files[ix].path().clone())
            .collect();
        // ponytail: the diff rebuilds once per file; add a batch setter to gpui-kit if big folders lag.
        self.diff.update(cx, |d, cx| {
            paths.iter().for_each(|p| d.set_file_collapsed(p, on, cx))
        });
        cx.notify();
    }

    fn toggle_viewed_path(&mut self, path: &str, cx: &mut Context<Self>) {
        if let Some(ix) = self.diff_files.iter().position(|f| f.path() == path) {
            self.toggle_viewed(ix, cx);
        }
    }

    fn rebuild_tree(&mut self) {
        let paths: Vec<String> = self
            .diff_files
            .iter()
            .map(|f| f.path().to_string())
            .collect();
        self.tree_rows = files::rows(&paths, &self.collapsed_dirs);
        self.tree_sel = self.tree_sel.min(self.tree_rows.len().saturating_sub(1));
    }

    /// Puts the tree cursor on file `ix`'s row, when it has one (not inside a folded folder).
    pub(super) fn follow_tree(&mut self, ix: usize) -> Option<usize> {
        let row = self.tree_rows.iter().position(|r| r.file == Some(ix))?;
        self.tree_sel = row;
        Some(row)
    }

    pub(super) fn tree_select(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(r) = self.tree_rows.get(row) else {
            return;
        };
        self.tree_sel = row;
        self.tree_scroll
            .scroll_to_item(row, ScrollStrategy::Nearest);
        if let Some(ix) = r.file {
            self.diff_file = ix;
            if self.tab == Tab::Diff {
                let path = self.diff_files[ix].path().to_string();
                self.review.at_file(&path);
                self.diff.update(cx, |d, cx| d.scroll_to_file(&path, cx));
            }
        }
        cx.notify();
    }

    fn set_dir_open(&mut self, open: Option<bool>, cx: &mut Context<Self>) {
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
        if open.unwrap_or(!is_open) == is_open {
            return;
        }
        match is_open {
            true => self.collapsed_dirs.insert(path),
            false => self.collapsed_dirs.remove(&path),
        };
        self.rebuild_tree();
        cx.notify();
    }

    /// Enter / click on a tree row: toggle a folder, show a file (in the diff, or the editor
    /// when one is open).
    fn tree_confirm(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.tree_select(row, cx);
        match self.tree_rows.get(row).and_then(|r| r.file) {
            None => self.set_dir_open(None, cx),
            Some(ix) if self.tab == Tab::File => self.open_file(ix, window, cx),
            Some(_) => {}
        }
    }

    /// The file a row action applies to: the tree cursor in the tree, the open file in the file
    /// view, else the file last moved to in the diff.
    pub(super) fn cursor_file(&self, window: &Window, cx: &App) -> Option<usize> {
        if self.tree_focus.is_focused(window) {
            return self.tree_rows.get(self.tree_sel)?.file;
        }
        if self.tab == Tab::File
            && let Some(e) = &self.editor
        {
            let path = &e.read(cx).path;
            return self.diff_files.iter().position(|f| f.path() == path);
        }
        // The diff can't say which file is on screen; use the last one moved to, which the
        // tree cursor follows.
        (self.diff_file < self.diff_files.len()).then_some(self.diff_file)
    }

    /// Loading, git failed, or nothing changed; `None` when there is a diff to show.
    pub(super) fn diff_placeholder(&self, cx: &App) -> Option<AnyElement> {
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
                "No changes",
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

    pub(super) fn render_diff(&self, cx: &mut Context<Self>) -> AnyElement {
        let viewed = self.viewed_now.clone();
        let shark = cx.entity().downgrade();
        let placeholder = self.diff_placeholder(cx);
        let empty = placeholder.is_some();
        let diff_with_annotations = self.with_comments(Diff::new(&self.diff), cx);
        div()
            .key_context("DiffPane")
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(self.viewport_probe())
            .on_scroll_wheel(cx.listener(|this, _, _, _| this.lose_view()))
            // The diff's own scrolling keys move it without the line cursor.
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _, _| {
                let k = ev.keystroke.key.as_str();
                if matches!(k, "up" | "down" | "pageup" | "pagedown" | "home" | "end") {
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
            .on_action(cx.listener(|this, _: &SendComments, w, cx| this.send_comments(w, cx)))
            .on_action(cx.listener(|this, _: &NextHunk, w, cx| this.diff_change(true, w, cx)))
            .on_action(cx.listener(|this, _: &PrevHunk, w, cx| this.diff_change(false, w, cx)))
            .on_action(cx.listener(|this, _: &NextFile, _, cx| this.step_file(1, cx)))
            .on_action(cx.listener(|this, _: &PrevFile, _, cx| this.step_file(-1, cx)))
            .on_action(cx.listener(|this, _: &RefreshDiff, _, cx| this.refresh_diff(cx)))
            .on_action(cx.listener(|this, _: &PlayMedia, w, cx| this.play_media(w, cx)))
            .on_action(cx.listener(|this, _: &ToggleDiffMode, _, cx| {
                this.diff.update(cx, |d, cx| {
                    let mode = if d.mode() == DiffMode::Split {
                        DiffMode::Unified
                    } else {
                        DiffMode::Split
                    };
                    d.set_mode(mode, cx)
                })
            }))
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
                    diff_with_annotations.render_header_prefix(move |file, _, _| {
                        // A prefix, not a suffix: the header is as wide as the longest line, so
                        // a suffix scrolls out of view.
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
            })
            .into_any_element()
    }

    pub(super) fn render_tree(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, muted) = (theme.border, theme.muted_foreground);
        let focused = self.tree_focus.is_focused(window);
        let n = self.diff_files.len();
        v_flex()
            .id("file-tree")
            .key_context("FileTree")
            .track_focus(&self.tree_focus)
            .w(px(260.))
            .flex_none()
            .h_full()
            .border_l_1()
            .border_color(border)
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
                h_flex()
                    .h(px(36.))
                    .flex_none()
                    .px_3()
                    .gap_2()
                    .text_xs()
                    .text_color(muted)
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Files"),
                    )
                    .child(format!("{}/{n} viewed", self.viewed_now.len())),
            )
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
                .flex_1()
                .pb_2(),
            )
    }

    fn render_tree_row(&self, ix: usize, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let r = &self.tree_rows[ix];
        let selected = ix == self.tree_sel;
        let open_file = self.editor.as_ref().map(|e| e.read(cx).path.clone());
        let row = h_flex()
            .id(("tree-row", ix))
            .h(px(28.))
            .w_full()
            .pr_2()
            .pl(px(8. + 12. * r.depth as f32))
            .gap_1p5()
            .rounded(theme.radius)
            .text_sm()
            .map(|d| cursor_row(d, selected, focused, theme.list_hover, theme))
            .on_click(cx.listener(move |this, e: &ClickEvent, w, cx| {
                this.tree_focus.focus(w, cx);
                match (e.click_count(), this.tree_rows.get(ix).and_then(|r| r.file)) {
                    (2, Some(file)) => this.open_file(file, w, cx),
                    _ => this.tree_confirm(ix, w, cx),
                }
            }));
        let Some(file) = r.file else {
            let open = !self.collapsed_dirs.contains(&r.path);
            return div()
                .px_2()
                .child(
                    row.text_color(theme.muted_foreground)
                        .child(
                            Icon::new(if open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .with_size(px(14.)),
                        )
                        .child(div().flex_1().truncate().child(r.name.clone()))
                        .child(self.dir_viewed_box(ix, &r.path, cx)),
                )
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
                    .with_size(px(14.))
                    .text_color(theme.muted_foreground),
            )
            .child(
                div()
                    .flex_1()
                    .truncate()
                    .text_color(match state {
                        Viewed::Yes => theme.muted_foreground,
                        _ => theme.foreground,
                    })
                    .when(is_open, |d| d.font_weight(FontWeight::MEDIUM))
                    .child(r.name.clone()),
            )
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
            .child(
                h_flex()
                    .gap_1()
                    .text_xs()
                    .when(f.additions() > 0, |d| {
                        d.child(
                            div()
                                .text_color(theme.success_foreground)
                                .child(format!("+{}", f.additions())),
                        )
                    })
                    .when(f.deletions() > 0, |d| {
                        d.child(
                            div()
                                .text_color(theme.danger_foreground)
                                .child(format!("−{}", f.deletions())),
                        )
                    }),
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
        div().px_2().child(row).into_any_element()
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

    fn step_file(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.diff_files.is_empty() {
            return;
        }
        let n = self.diff_files.len() as isize;
        self.diff_file = (self.diff_file as isize + delta).rem_euclid(n) as usize;
        let path = self.diff_files[self.diff_file].path().to_string();
        self.review.at_file(&path);
        self.diff.update(cx, |d, cx| d.scroll_to_file(&path, cx));
        if let Some(row) = self.follow_tree(self.diff_file) {
            self.tree_scroll
                .scroll_to_item(row, ScrollStrategy::Nearest);
        }
        cx.notify();
    }
}
