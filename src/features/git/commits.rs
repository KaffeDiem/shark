//! Git's Commits view: the branch's own commits (`base..HEAD`), read from immutable Git
//! objects. It never checks out, stages or edits; working files and Diff stay untouched.
use super::document::{DocContext, RichDoc};
use crate::diff_view::{Diff, DiffFile, DiffMode, DiffState};
use crate::git::history::{self, Blob, Commit, FileChange, Summary};
use crate::media::{self, Lru, MediaFile, MediaView, Side, Source, human_bytes};
use crate::{
    CancelEdit, Confirm, PageDown, PageUp, SelectFirst, SelectLast, SelectNext, SelectPrev,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme, Icon, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::PathBuf;
use std::rc::Rc;

/// What the history is read from: the host's effective comparison base, as Diff uses it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RepoContext {
    pub(crate) wt: PathBuf,
    pub(crate) base: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum FileView {
    Diff,
    Before,
    After,
}

/// Files above this are not read for a text view.
const MAX_TEXT: u64 = 16 * 1024 * 1024;

enum Content {
    Loading,
    Failed(String),
    /// An explicit entry with nothing to draw: a missing side, submodule, binary, mode change.
    Note(String),
    Diff,
    Text {
        text: String,
        markdown: bool,
    },
    Media(Entity<MediaView>),
}

pub(crate) struct Commits {
    repo: Option<RepoContext>,
    web: Option<String>,
    generation: u64,
    summaries: Vec<Summary>,
    loading: bool,
    loaded: bool,
    error: Option<String>,
    cursor: usize,
    selected: Option<String>,
    commit: Option<Commit>,
    commit_error: Option<String>,
    file_cursor: usize,
    file: Option<usize>,
    view: FileView,
    source: bool,
    content: Option<Content>,
    content_generation: u64,
    editor: Option<Entity<EditorState>>,
    doc: Option<Entity<RichDoc>>,
    diff: Entity<DiffState>,
    cache: Entity<Lru>,
    list: ListState,
    files_list: ListState,
    focus: FocusHandle,
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

fn when(date: &str) -> String {
    date.replacen('T', " ", 1).chars().take(16).collect()
}

fn markdown(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".md", ".markdown", ".mdx", ".mdown"]
        .iter()
        .any(|e| lower.ends_with(e))
}

fn status_label(f: &FileChange) -> &'static str {
    match f.status {
        'A' => "Added",
        'D' => "Deleted",
        'T' => "Type changed",
        _ if f.mode_only().is_some() => "Mode changed",
        _ => "Modified",
    }
}

impl Commits {
    pub(crate) fn new(cache: Entity<Lru>, cx: &mut Context<Self>) -> Self {
        let diff = cx.new(|cx| DiffState::new(vec![], cx).with_mode(DiffMode::Unified));
        Self {
            repo: None,
            web: None,
            generation: 0,
            summaries: vec![],
            loading: false,
            loaded: false,
            error: None,
            cursor: 0,
            selected: None,
            commit: None,
            commit_error: None,
            file_cursor: 0,
            file: None,
            view: FileView::Diff,
            source: false,
            content: None,
            content_generation: 0,
            editor: None,
            doc: None,
            diff,
            cache,
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            files_list: ListState::new(0, ListAlignment::Top, px(400.)),
            focus: cx.focus_handle(),
        }
    }

    pub(crate) fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// A different repository or base discards the history; it reloads when next shown.
    pub(crate) fn set_repo(&mut self, repo: Option<RepoContext>, cx: &mut Context<Self>) {
        if self.repo == repo {
            return;
        }
        self.repo = repo;
        self.generation += 1;
        self.leave_file(cx);
        self.summaries.clear();
        self.loading = false;
        self.loaded = false;
        self.error = None;
        self.cursor = 0;
        self.selected = None;
        self.commit = None;
        self.commit_error = None;
        self.list.reset(0);
        cx.notify();
    }

    pub(crate) fn ensure_loaded(&mut self, cx: &mut Context<Self>) {
        if !self.loaded && !self.loading {
            self.refresh(cx);
        }
    }

    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        self.generation += 1;
        let generation = self.generation;
        self.loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let list = history::commits(&repo.wt, &repo.base)?;
                    let web = crate::git::git(&repo.wt, &["config", "--get", "remote.origin.url"])
                        .ok()
                        .and_then(|u| crate::sync::github_url(&u));
                    Ok::<_, String>((list, web))
                })
                .await;
            this.update(cx, |s, cx| {
                if s.generation != generation {
                    return;
                }
                s.loading = false;
                match result {
                    Ok((list, web)) => {
                        s.loaded = true;
                        s.error = None;
                        s.web = web;
                        s.summaries = list;
                        s.list.reset(s.summaries.len());
                        s.cursor = s.cursor.min(s.summaries.len().saturating_sub(1));
                        // Selections stay while their commit is still part of the branch.
                        if let Some(sha) = &s.selected
                            && !s.summaries.iter().any(|c| &c.sha == sha)
                        {
                            s.close_commit(cx);
                        }
                    }
                    // Failed refreshes keep what loaded before.
                    Err(e) => s.error = Some(e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn open_commit(&mut self, index: usize, cx: &mut Context<Self>) {
        let (Some(repo), Some(summary)) = (self.repo.clone(), self.summaries.get(index)) else {
            return;
        };
        let sha = summary.sha.clone();
        self.cursor = index;
        if self.selected.as_ref() == Some(&sha) && self.commit.is_some() {
            return;
        }
        self.leave_file(cx);
        self.selected = Some(sha.clone());
        self.commit = None;
        self.commit_error = None;
        self.file_cursor = 0;
        self.files_list.reset(1);
        let generation = self.generation;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let wanted = sha.clone();
            let result = cx
                .background_executor()
                .spawn(async move { history::commit(&repo.wt, &wanted) })
                .await;
            this.update(cx, |s, cx| {
                if s.generation != generation || s.selected.as_ref() != Some(&sha) {
                    return;
                }
                match result {
                    Ok(c) => {
                        s.files_list.reset(1 + c.files.len().max(1));
                        s.commit = Some(c);
                    }
                    Err(e) => s.commit_error = Some(e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn close_commit(&mut self, cx: &mut Context<Self>) {
        self.leave_file(cx);
        self.selected = None;
        self.commit = None;
        self.commit_error = None;
        cx.notify();
    }

    /// Older is further down the newest-first list.
    fn step(&mut self, older: bool, cx: &mut Context<Self>) {
        let Some(at) = self
            .selected
            .as_ref()
            .and_then(|sha| self.summaries.iter().position(|c| &c.sha == sha))
        else {
            return;
        };
        match older {
            true if at + 1 < self.summaries.len() => self.open_commit(at + 1, cx),
            false if at > 0 => self.open_commit(at - 1, cx),
            _ => {}
        }
    }

    /// Stops playback and drops what the open file showed.
    fn leave_file(&mut self, cx: &mut Context<Self>) {
        self.stop(cx);
        self.content_generation += 1;
        self.content = None;
        self.editor = None;
        self.doc = None;
        self.file = None;
    }

    /// Stops any video, when the view or the file is left.
    pub(crate) fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(Content::Media(v)) = &self.content {
            v.update(cx, |v, cx| v.stop(cx));
        }
        if let Some(doc) = &self.doc {
            doc.update(cx, |d, cx| d.stop(cx));
        }
    }

    fn open_file(&mut self, index: usize, view: FileView, cx: &mut Context<Self>) {
        let (Some(repo), Some(commit)) = (self.repo.clone(), self.commit.clone()) else {
            return;
        };
        let Some(file) = commit.files.get(index).cloned() else {
            return;
        };
        self.leave_file(cx);
        // Added and deleted files have one side only.
        let view = match (view, &file.old, &file.new) {
            (FileView::Before, None, _) => FileView::After,
            (FileView::After, _, None) => FileView::Before,
            (v, _, _) => v,
        };
        self.file = Some(index);
        self.file_cursor = index;
        self.view = view;
        self.content = Some(Content::Loading);
        let generation = self.content_generation;
        cx.notify();
        let media = media::kind(&file.path)
            .filter(|_| file.special().is_none() && file.mode_only().is_none());
        if let Some(kind) = media {
            let side = |b: &Option<Blob>| {
                b.as_ref().map(|b| Side {
                    sha: b.id.clone(),
                    repo: repo.wt.clone(),
                    source: Source::Blob,
                })
            };
            let mf = MediaFile {
                path: file.path.clone().into(),
                kind,
                old: side(&file.old),
                new: side(&file.new),
            };
            let width = Rc::new(std::cell::Cell::new(px(0.)));
            let cache = self.cache.clone();
            let view = cx.new(|_| {
                let v = MediaView::new(mf, cache, width);
                match view {
                    FileView::Diff => v,
                    FileView::Before => v.only(0, "Before"),
                    FileView::After => v.only(1, "After"),
                }
            });
            self.content = Some(Content::Media(view));
            return;
        }
        cx.spawn(async move |this, cx| {
            let wt = repo.wt.clone();
            let f = file.clone();
            let c = commit.clone();
            let result = cx
                .background_executor()
                .spawn(async move { load(&wt, &c, &f, view) })
                .await;
            this.update(cx, |s, cx| {
                if s.content_generation != generation {
                    return;
                }
                s.content = Some(match result {
                    Ok(Loaded::Patch(d)) => {
                        s.diff.update(cx, |state, cx| state.set_files(d, cx));
                        Content::Diff
                    }
                    Ok(Loaded::Note(n)) => Content::Note(n),
                    Ok(Loaded::Text(text)) => Content::Text {
                        markdown: markdown(&file.path),
                        text,
                    },
                    Err(e) => Content::Failed(e),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_view(&mut self, view: FileView, cx: &mut Context<Self>) {
        if let Some(i) = self.file
            && self.view != view
        {
            self.open_file(i, view, cx);
        }
    }

    fn doc_context(&self, file: &FileChange) -> Option<DocContext> {
        let (repo, commit) = (self.repo.as_ref()?, self.commit.as_ref()?);
        let rev = match self.view {
            FileView::Before => commit.compared_to.clone(),
            _ => Some(commit.sha.clone()),
        };
        Some(DocContext {
            repo: repo.wt.clone(),
            rev,
            dir: file
                .path
                .rsplit_once('/')
                .map(|(d, _)| d.to_string())
                .unwrap_or_default(),
            web: self.web.clone(),
        })
    }

    fn back(&mut self, cx: &mut Context<Self>) -> bool {
        if self.file.is_some() {
            self.leave_file(cx);
        } else if self.selected.is_some() {
            self.close_commit(cx);
        } else {
            return false;
        }
        cx.notify();
        true
    }

    fn move_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.file.is_some() {
            return self.scroll(delta as f32 * 2., cx);
        }
        let (cursor, len, list) = match &self.commit {
            Some(c) if self.selected.is_some() => {
                (&mut self.file_cursor, c.files.len(), &self.files_list)
            }
            _ => (&mut self.cursor, self.summaries.len(), &self.list),
        };
        if len == 0 {
            return;
        }
        *cursor = (*cursor as isize + delta).clamp(0, len as isize - 1) as usize;
        let row = *cursor + usize::from(self.selected.is_some());
        list.scroll_to_reveal_item(row);
        cx.notify();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if self.file.is_some() {
            return;
        }
        if self.selected.is_some() {
            if self
                .commit
                .as_ref()
                .is_some_and(|c| self.file_cursor < c.files.len())
            {
                self.open_file(self.file_cursor, FileView::Diff, cx);
            }
        } else if self.cursor < self.summaries.len() {
            self.open_commit(self.cursor, cx);
        }
    }

    /// The open file's scrolling list, when it shows one: the diff, or the Markdown preview
    /// (kept, hidden, under Source). Plain text is an editor, focused when it opens.
    fn file_list(&self, cx: &App) -> Option<ListState> {
        match &self.content {
            Some(Content::Diff) => Some(self.diff.read(cx).list().clone()),
            _ if !self.source => self.doc.as_ref().and_then(|d| d.read(cx).list().cloned()),
            _ => None,
        }
    }

    /// Home and End: the open file's first or last line, else the first or last row.
    fn edge(&mut self, end: bool, cx: &mut Context<Self>) {
        match self.file_list(cx) {
            Some(l) if end => l.scroll_to_end(),
            Some(l) => l.scroll_to(ListOffset::default()),
            None if self.file.is_some() => {}
            None => self.move_cursor(if end { isize::MAX / 2 } else { isize::MIN / 2 }, cx),
        }
        cx.notify();
    }

    fn scroll(&mut self, lines: f32, cx: &mut Context<Self>) {
        let delta = px(lines * 21.);
        if let Some(l) = self.file_list(cx) {
            l.scroll_by(delta);
        } else if self.selected.is_some() && self.file.is_none() {
            self.files_list.scroll_by(delta);
        } else if self.file.is_none() {
            self.list.scroll_by(delta);
        }
        cx.notify();
    }

    fn render_commit_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let Some(c) = self.summaries.get(ix) else {
            return div().into_any_element();
        };
        let selected = ix == self.cursor;
        v_flex()
            .id(("git-commit", ix))
            .test_support()
            .px_4()
            .py_2()
            .gap_0p5()
            .cursor_pointer()
            .border_b_1()
            .border_color(theme.border.opacity(0.5))
            .when(selected, |d| d.bg(theme.secondary))
            .hover(|d| d.bg(theme.secondary.opacity(0.6)))
            .on_click(cx.listener(move |s, _, w, cx| {
                s.open_commit(ix, cx);
                s.focus.focus(w, cx);
            }))
            .child(
                h_flex()
                    .gap_2()
                    .min_w_0()
                    .when(c.merge, |d| {
                        d.child(
                            Icon::new(IconName::GitMerge)
                                .with_size(px(14.))
                                .text_color(muted),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(if c.subject.is_empty() {
                                "(no message)".to_string()
                            } else {
                                c.subject.clone()
                            }),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(muted)
                    .child(
                        div()
                            .font_family(theme.mono_font_family.clone())
                            .child(short(&c.sha).to_string()),
                    )
                    .child(div().min_w_0().truncate().child(c.author.clone()))
                    .child(when(&c.date)),
            )
            .into_any_element()
    }

    fn render_commit_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let at = self
            .selected
            .as_ref()
            .and_then(|sha| self.summaries.iter().position(|c| &c.sha == sha));
        let nav = h_flex()
            .gap_1()
            .flex_wrap()
            .child(
                Button::new("git-commit-back")
                    .icon(IconName::ArrowLeft)
                    .label("All commits")
                    .small()
                    .ghost()
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.close_commit(cx);
                    })),
            )
            .child(div().flex_1())
            .child(
                Button::new("git-commit-newer")
                    .icon(IconName::ChevronUp)
                    .label("Newer")
                    .small()
                    .ghost()
                    .disabled(at.is_none_or(|a| a == 0))
                    .on_click(cx.listener(|s, _, _, cx| s.step(false, cx))),
            )
            .child(
                Button::new("git-commit-older")
                    .icon(IconName::ChevronDown)
                    .label("Older")
                    .small()
                    .ghost()
                    .disabled(at.is_none_or(|a| a + 1 >= self.summaries.len()))
                    .on_click(cx.listener(|s, _, _, cx| s.step(true, cx))),
            );
        let body = match (&self.commit, &self.commit_error) {
            (_, Some(e)) => div()
                .id("git-commit-error")
                .test_support()
                .text_sm()
                .text_color(muted)
                .child(format!("Could not read this commit: {e}"))
                .into_any_element(),
            (None, None) => h_flex()
                .gap_2()
                .text_sm()
                .text_color(muted)
                .child(Spinner::new().color(muted))
                .child("Reading commit…")
                .into_any_element(),
            (Some(c), None) => {
                let (subject, rest) = c.message.split_once('\n').unwrap_or((&c.message, ""));
                let comparison = match &c.compared_to {
                    None => "Root commit · compared with an empty tree".to_string(),
                    Some(p) if c.merge() => {
                        format!("Merge commit · compared with its first parent {}", short(p))
                    }
                    Some(p) => format!("Compared with parent {}", short(p)),
                };
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .id("git-commit-subject")
                            .test_support()
                            .text_size(px(18.))
                            .line_height(relative(1.3))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(subject.to_string()),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .text_xs()
                            .text_color(muted)
                            .child(
                                div()
                                    .font_family(theme.mono_font_family.clone())
                                    .child(c.sha.clone()),
                            )
                            .child(format!("{} <{}>", c.author, c.email))
                            .child(when(&c.date)),
                    )
                    .child(
                        div()
                            .id("git-commit-comparison")
                            .test_support()
                            .text_xs()
                            .text_color(muted)
                            .child(comparison),
                    )
                    .when(!rest.trim().is_empty(), |d| {
                        d.child(
                            div()
                                .id("git-commit-message")
                                .test_support()
                                .text_sm()
                                .whitespace_normal()
                                .child(rest.trim().to_string()),
                        )
                    })
                    .child(
                        div()
                            .pt_2()
                            .text_xs()
                            .text_color(muted)
                            .font_weight(FontWeight::MEDIUM)
                            .child(match c.files.len() {
                                1 => "1 changed file".to_string(),
                                n => format!("{n} changed files"),
                            }),
                    )
                    .into_any_element()
            }
        };
        v_flex()
            .px_4()
            .pt_2()
            .pb_3()
            .gap_3()
            .child(nav)
            .child(body)
            .into_any_element()
    }

    fn render_file_row(&mut self, row: usize, cx: &mut Context<Self>) -> AnyElement {
        if row == 0 {
            return self.render_commit_header(cx);
        }
        let ix = row - 1;
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let Some(c) = &self.commit else {
            return div().into_any_element();
        };
        let Some(f) = c.files.get(ix) else {
            return div()
                .id("git-commit-empty")
                .test_support()
                .px_4()
                .py_3()
                .text_sm()
                .text_color(muted)
                .child("This commit changes no files.")
                .into_any_element();
        };
        let mut notes = vec![status_label(f).to_string()];
        if let Some((o, n)) = f.mode_change() {
            notes.push(format!("{o} → {n}"));
        }
        notes.extend(f.special().map(String::from));
        let color = match f.status {
            'A' => theme.success_foreground,
            'D' => theme.danger,
            _ => theme.warning_foreground,
        };
        h_flex()
            .id(("git-commit-file", ix))
            .test_support()
            .px_4()
            .py_1p5()
            .gap_2()
            .cursor_pointer()
            .when(ix == self.file_cursor, |d| d.bg(theme.secondary))
            .hover(|d| d.bg(theme.secondary.opacity(0.6)))
            .on_click(cx.listener(move |s, _, _, cx| s.open_file(ix, FileView::Diff, cx)))
            .child(
                div()
                    .w(px(14.))
                    .flex_none()
                    .text_xs()
                    .font_weight(FontWeight::BOLD)
                    .text_color(color)
                    .child(f.status.to_string()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_family(theme.mono_font_family.clone())
                    .child(f.path.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(muted)
                    .child(notes.join(" · ")),
            )
            .into_any_element()
    }

    fn render_file(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (Some(ix), Some(commit)) = (self.file, self.commit.clone()) else {
            return div().into_any_element();
        };
        let file = commit.files[ix].clone();
        // Created here, where a window is at hand; the content itself loaded off-thread.
        if let Some(Content::Text { text, markdown }) = &self.content {
            if (!*markdown || self.source) && self.editor.is_none() {
                let text = text.clone();
                let input = cx.new(|cx| {
                    EditorState::new(window, cx)
                        .default_value(text)
                        .soft_wrap(true)
                });
                input.update(cx, |i, cx| i.set_readonly(true, cx));
                // It has no scroll API of its own, so keys reach it by focus.
                if self.focus.contains_focused(window, cx) {
                    input.read(cx).focus_handle(cx).focus(window, cx);
                }
                self.editor = Some(input);
            }
            if *markdown && !self.source && self.doc.is_none() {
                let doc = RichDoc::new(
                    text.clone(),
                    self.doc_context(&file),
                    self.cache.clone(),
                    true,
                );
                self.doc = Some(cx.new(|_| doc));
            }
        }
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let is_markdown = matches!(&self.content, Some(Content::Text { markdown: true, .. }));
        let view_button = |id: &'static str, label: &'static str, view: FileView, enabled: bool| {
            Button::new(id)
                .label(label)
                .small()
                .ghost()
                .selected(self.view == view)
                .disabled(!enabled)
                .on_click(cx.listener(move |s, _, _, cx| s.set_view(view, cx)))
        };
        let toolbar = v_flex()
            .px_4()
            .py_2()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("git-file-back")
                            .icon(IconName::ArrowLeft)
                            .label(short(&commit.sha).to_string())
                            .tooltip("Back to the commit")
                            .small()
                            .ghost()
                            .on_click(cx.listener(|s, _, _, cx| {
                                s.leave_file(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .id("git-file-path")
                            .test_support()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_family(theme.mono_font_family.clone())
                            .child(file.path.clone()),
                    ),
            )
            .child(
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .child(view_button("git-file-diff", "Diff", FileView::Diff, true))
                    .child(view_button(
                        "git-file-before",
                        "Before",
                        FileView::Before,
                        file.old.is_some(),
                    ))
                    .child(view_button(
                        "git-file-after",
                        "After",
                        FileView::After,
                        file.new.is_some(),
                    ))
                    .when(is_markdown && self.view != FileView::Diff, |d| {
                        d.child(div().w(px(12.)))
                            .child(
                                Button::new("git-file-preview")
                                    .label("Preview")
                                    .small()
                                    .ghost()
                                    .selected(!self.source)
                                    .on_click(cx.listener(|s, _, w, cx| {
                                        s.source = false;
                                        s.focus.focus(w, cx);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("git-file-source")
                                    .label("Source")
                                    .small()
                                    .ghost()
                                    .selected(self.source)
                                    .on_click(cx.listener(|s, _, _, cx| {
                                        if let Some(doc) = &s.doc {
                                            doc.update(cx, |d, cx| d.stop(cx));
                                        }
                                        s.source = true;
                                        cx.notify();
                                    })),
                            )
                    })
                    .child(div().flex_1())
                    .child(
                        div().text_xs().text_color(muted).child(match self.view {
                            FileView::Diff => match &commit.compared_to {
                                Some(p) => format!("{} → {}", short(p), short(&commit.sha)),
                                None => format!("empty → {}", short(&commit.sha)),
                            },
                            FileView::Before => commit
                                .compared_to
                                .as_deref()
                                .map(|p| format!("at {}", short(p)))
                                .unwrap_or_default(),
                            FileView::After => format!("at {}", short(&commit.sha)),
                        }),
                    ),
            );
        let note = |id: &'static str, text: String| {
            div()
                .id(id)
                .test_support()
                .p_4()
                .text_sm()
                .text_color(muted)
                .whitespace_normal()
                .child(text)
                .into_any_element()
        };
        let body = match &self.content {
            None | Some(Content::Loading) => h_flex()
                .p_4()
                .gap_2()
                .text_sm()
                .text_color(muted)
                .child(Spinner::new().color(muted))
                .child("Reading from Git…")
                .into_any_element(),
            Some(Content::Failed(e)) => {
                note("git-file-error", format!("Could not read this file: {e}"))
            }
            Some(Content::Note(n)) => note("git-file-note", n.clone()),
            Some(Content::Diff) => div()
                .id("git-file-diff-view")
                .test_support()
                .size_full()
                .child(Diff::new(&self.diff).size_full())
                .into_any_element(),
            Some(Content::Media(v)) => div()
                .id("git-file-media")
                .test_support()
                .p_4()
                .size_full()
                .overflow_y_scroll()
                .child(v.clone())
                .into_any_element(),
            Some(Content::Text { markdown, .. }) => match (&self.doc, &self.editor) {
                (Some(doc), _) if *markdown && !self.source => div()
                    .id("git-file-preview-view")
                    .test_support()
                    .size_full()
                    .child(doc.clone())
                    .into_any_element(),
                (_, Some(input)) => div()
                    .id("git-file-text")
                    .test_support()
                    .size_full()
                    .child(Editor::new(input).readonly(true).appearance(false).h_full())
                    .into_any_element(),
                _ => div().into_any_element(),
            },
        };
        v_flex()
            .size_full()
            .min_h_0()
            .child(toolbar)
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }
}

enum Loaded {
    Patch(Vec<DiffFile>),
    Note(String),
    Text(String),
}

/// A blob's size for a note: empty when unreadable, `none` for a missing side.
fn size_of(wt: &std::path::Path, b: &Option<Blob>) -> String {
    b.as_ref().map_or("none".into(), |b| {
        history::blob_size(wt, &b.id)
            .map(human_bytes)
            .unwrap_or_default()
    })
}

/// Reads one view of a file from Git objects. Blocking: background only.
fn load(
    wt: &std::path::Path,
    commit: &Commit,
    file: &FileChange,
    view: FileView,
) -> Result<Loaded, String> {
    if view == FileView::Diff {
        if let Some((o, n)) = file.mode_only() {
            return Ok(Loaded::Note(format!(
                "File mode changed from {o} to {n}; the contents are unchanged."
            )));
        }
        if file.special() == Some("submodule") {
            let side = |b: &Option<Blob>| {
                b.as_ref()
                    .map_or("none".into(), |b| short(&b.id).to_string())
            };
            return Ok(Loaded::Note(format!(
                "Submodule {}: {} → {}",
                file.path,
                side(&file.old),
                side(&file.new)
            )));
        }
        for b in [&file.old, &file.new].into_iter().flatten() {
            let size = history::blob_size(wt, &b.id)?;
            if size > MAX_TEXT {
                return Ok(Loaded::Note(format!(
                    "Too large to diff here ({}).",
                    human_bytes(size)
                )));
            }
        }
        let patch = history::patch(wt, commit, &file.path)?;
        // A type change (file ↔ symlink) is two entries: a deletion and an addition.
        let parsed = DiffFile::parse(&patch).map_err(|e| e.to_string())?;
        return match parsed.is_empty() {
            true => Ok(Loaded::Note(
                "Git reports no textual change for this file.".into(),
            )),
            false if parsed.iter().all(|d| d.is_binary()) => Ok(Loaded::Note(format!(
                "Binary file with no inline preview ({} → {}).",
                size_of(wt, &file.old),
                size_of(wt, &file.new),
            ))),
            false => Ok(Loaded::Patch(parsed)),
        };
    }
    let blob = match view {
        FileView::Before => file.old.as_ref(),
        _ => file.new.as_ref(),
    };
    let Some(blob) = blob else {
        return Ok(Loaded::Note(match view {
            FileView::Before => "There is no earlier version: this commit added the file.".into(),
            _ => "There is no later version: this commit deleted the file.".into(),
        }));
    };
    if blob.submodule() {
        return Ok(Loaded::Note(format!("Submodule at commit {}.", blob.id)));
    }
    let size = history::blob_size(wt, &blob.id)?;
    if blob.symlink() {
        let target = history::blob(wt, &blob.id)?;
        return Ok(Loaded::Note(format!(
            "Symbolic link to {}. Links are shown, never followed.",
            String::from_utf8_lossy(&target)
        )));
    }
    if size > MAX_TEXT {
        return Ok(Loaded::Note(format!(
            "Too large to show here ({}).",
            human_bytes(size)
        )));
    }
    let bytes = history::blob(wt, &blob.id)?;
    let binary = bytes[..bytes.len().min(8000)].contains(&0);
    match String::from_utf8(bytes) {
        Ok(text) if !binary => Ok(Loaded::Text(text)),
        _ => Ok(Loaded::Note(format!(
            "Binary file ({}); it has no text preview.",
            human_bytes(size)
        ))),
    }
}

impl Render for Commits {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = if self.file.is_some() {
            self.render_file(window, cx)
        } else if self.selected.is_some() {
            list(
                self.files_list.clone(),
                cx.processor(|s, i, _, cx| s.render_file_row(i, cx)),
            )
            .size_full()
            .into_any_element()
        } else if !self.summaries.is_empty() {
            let theme = cx.theme();
            let muted = theme.muted_foreground;
            let count = self.summaries.len();
            let capped = count == history::MAX_COMMITS;
            v_flex()
                .size_full()
                .child(
                    h_flex()
                        .px_4()
                        .py_2()
                        .gap_2()
                        .text_xs()
                        .text_color(muted)
                        .border_b_1()
                        .border_color(theme.border)
                        .child(div().flex_1().min_w_0().truncate().child(format!(
                            "{count}{} commit{} on this branch since {}",
                            if capped { "+" } else { "" },
                            if count == 1 { "" } else { "s" },
                            self.repo.as_ref().map(|r| r.base.as_str()).unwrap_or("base"),
                        )))
                        .child(
                            Button::new("git-commits-refresh")
                                .icon(IconName::RefreshCw)
                                .loading(self.loading)
                                .xsmall()
                                .ghost()
                                .tooltip("Refresh commits")
                                .disabled(self.loading)
                                .on_click(cx.listener(|s, _, _, cx| s.refresh(cx))),
                        ),
                )
                .child(
                    list(
                        self.list.clone(),
                        cx.processor(|s, i, _, cx| s.render_commit_row(i, cx)),
                    )
                    .flex_1()
                    .min_h_0(),
                )
                .into_any_element()
        } else {
            let theme = cx.theme();
            let muted = theme.muted_foreground;
            let (id, title, detail) = if self.repo.is_none() {
                (
                    "git-commits-none",
                    "No repository",
                    "Add a repository to browse its commits.",
                )
            } else if self.loading || !self.loaded && self.error.is_none() {
                (
                    "git-commits-loading",
                    "Reading commits…",
                    "Listing this branch’s commits since its comparison base.",
                )
            } else if self.error.is_some() {
                ("git-commits-error", "Could not read the commits", "")
            } else {
                (
                    "git-commits-empty",
                    "No commits yet",
                    "This branch has no commits beyond its comparison base.",
                )
            };
            v_flex()
                .id(id)
                .test_support()
                .size_full()
                .items_center()
                .px_4()
                .py_8()
                .gap_3()
                .text_center()
                .text_size(px(14.))
                .child(if self.loading {
                    Spinner::new()
                        .with_size(px(28.))
                        .color(muted)
                        .into_any_element()
                } else {
                    Icon::new(IconName::GitBranch)
                        .with_size(px(28.))
                        .text_color(muted)
                        .into_any_element()
                })
                .child(div().text_lg().font_weight(FontWeight::MEDIUM).child(title))
                .when(!detail.is_empty(), |d| {
                    d.child(div().text_color(muted).child(detail))
                })
                .when_some(self.error.clone(), |d, e| {
                    d.child(
                        div()
                            .id("git-commits-error-detail")
                            .test_support()
                            .max_w(px(440.))
                            .p_3()
                            .rounded(theme.radius)
                            .bg(theme.secondary)
                            .text_sm()
                            .text_left()
                            .text_color(muted)
                            .child(e),
                    )
                })
                .when(self.repo.is_some() && !self.loading, |d| {
                    d.child(
                        Button::new("git-commits-retry")
                            .icon(IconName::RefreshCw)
                            .label("Try again")
                            .small()
                            .on_click(cx.listener(|s, _, _, cx| s.refresh(cx))),
                    )
                })
                .into_any_element()
        };
        let warning = cx.theme().warning_foreground;
        let error = self
            .error
            .clone()
            .filter(|_| !self.summaries.is_empty() && self.file.is_none());
        v_flex()
            .id("git-commits")
            .key_context("GitCommits")
            .track_focus(&self.focus)
            .test_support()
            .size_full()
            .min_h_0()
            .on_action(cx.listener(|s, _: &SelectNext, _, cx| s.move_cursor(1, cx)))
            .on_action(cx.listener(|s, _: &SelectPrev, _, cx| s.move_cursor(-1, cx)))
            .on_action(cx.listener(|s, _: &PageDown, _, cx| s.scroll(15., cx)))
            .on_action(cx.listener(|s, _: &PageUp, _, cx| s.scroll(-15., cx)))
            .on_action(cx.listener(|s, _: &SelectFirst, _, cx| s.edge(false, cx)))
            .on_action(cx.listener(|s, _: &SelectLast, _, cx| s.edge(true, cx)))
            .on_action(cx.listener(|s, _: &Confirm, _, cx| s.confirm(cx)))
            .on_action(cx.listener(|s, _: &CancelEdit, w, cx| {
                if s.back(cx) {
                    s.focus.focus(w, cx);
                } else {
                    cx.propagate();
                }
            }))
            .when_some(error, |d, e| {
                d.child(
                    h_flex()
                        .id("git-commits-stale")
                        .test_support()
                        .px_4()
                        .py_2()
                        .gap_2()
                        .text_sm()
                        .child(
                            Icon::new(IconName::TriangleAlert)
                                .with_size(px(16.))
                                .text_color(warning)
                                .flex_none(),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .child(format!("Showing previously loaded commits. {e}")),
                        ),
                )
            })
            .child(div().flex_1().min_h_0().child(content))
    }
}

#[cfg(test)]
impl Commits {
    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }
    pub(crate) fn summaries(&self) -> &[Summary] {
        &self.summaries
    }
    pub(crate) fn commit(&self) -> Option<&Commit> {
        self.commit.as_ref()
    }
    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    pub(crate) fn open_file_for_test(
        &mut self,
        index: usize,
        view: FileView,
        cx: &mut Context<Self>,
    ) {
        self.open_file(index, view, cx);
    }
    pub(crate) fn set_source(&mut self, source: bool, cx: &mut Context<Self>) {
        self.source = source;
        cx.notify();
    }
    pub(crate) fn step_for_test(&mut self, older: bool, cx: &mut Context<Self>) {
        self.step(older, cx);
    }
    /// What the open file shows: `diff`, `media`, `note:<text>`, `text:<contents>` or `loading`.
    pub(crate) fn shown(&self, cx: &App) -> String {
        match &self.content {
            None => "none".into(),
            Some(Content::Loading) => "loading".into(),
            Some(Content::Failed(e)) => format!("error:{e}"),
            Some(Content::Note(n)) => format!("note:{n}"),
            Some(Content::Diff) => {
                let files = self.diff.read(cx).files();
                format!(
                    "diff:{}",
                    files
                        .first()
                        .map(|f| f.path().to_string())
                        .unwrap_or_default()
                )
            }
            Some(Content::Text { text, .. }) => format!("text:{text}"),
            Some(Content::Media(_)) => "media".into(),
        }
    }
    pub(crate) fn media(&self) -> Option<&Entity<MediaView>> {
        match &self.content {
            Some(Content::Media(v)) => Some(v),
            _ => None,
        }
    }
    pub(crate) fn doc(&self) -> Option<&Entity<RichDoc>> {
        self.doc.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file replaced by a symlink is a deletion plus an addition in the patch; Diff shows both.
    #[::core::prelude::v1::test]
    fn type_change_diff_keeps_both_entries() {
        let dir = std::env::temp_dir().join(format!("shark-typechange-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let o = std::process::Command::new("git")
                .current_dir(&dir)
                .args(["-c", "user.name=T", "-c", "user.email=t@e"])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .output()
                .unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        std::fs::write(dir.join("f"), "text\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "one"]);
        std::fs::remove_file(dir.join("f")).unwrap();
        std::os::unix::fs::symlink("target", dir.join("f")).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "two"]);
        let c = history::commit(&dir, &git(&["rev-parse", "HEAD"])).unwrap();
        let got = load(&dir, &c, &c.files[0], FileView::Diff);
        let _ = std::fs::remove_dir_all(&dir);
        match got {
            Ok(Loaded::Patch(d)) => assert_eq!(d.len(), 2),
            _ => panic!("expected a two-entry patch"),
        }
    }
}
