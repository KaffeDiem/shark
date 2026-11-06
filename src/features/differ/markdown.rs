//! Read either complete side of a Markdown patch using the shared document reader.
use super::*;
use crate::diff_view::DiffSide;
use crate::features::git::document::{DocContext, RichDoc, is_markdown};
use gpui_kit::component::{Disableable as _, Selectable as _};

pub(super) struct Preview {
    pub(super) path: SharedString,
    side: DiffSide,
    doc: Entity<RichDoc>,
}

impl Differ {
    pub(super) fn markdown_button(
        &self,
        file: &DiffFile,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !is_markdown(file.path())
            || file.is_binary()
            || self.editing.is_some()
            || (file.lines_count(DiffSide::Original) == 0
                && file.lines_count(DiffSide::Modified) == 0)
        {
            return None;
        }
        let path = file.path().clone();
        Some(
            Button::new(SharedString::from(format!("markdown-preview:{path}")))
                .label("Preview")
                .tooltip("Read formatted Markdown")
                .small()
                .ghost()
                .on_click(cx.listener(move |s, _, w, cx| {
                    s.open_markdown(&path, DiffSide::Modified, cx);
                    s.focus_diff(w, cx);
                }))
                .into_any_element(),
        )
    }

    pub(super) fn open_markdown(&mut self, path: &str, side: DiffSide, cx: &mut Context<Self>) {
        let Some(file) = self.diff_files.iter().find(|f| f.path() == path) else {
            return;
        };
        if !is_markdown(path) || file.is_binary() || self.editing.is_some() {
            return;
        }
        let side = match file.status() {
            DiffFileStatus::Added => DiffSide::Modified,
            DiffFileStatus::Deleted => DiffSide::Original,
            _ => side,
        };
        // combined_diff supplies whole files; use the captured patch, not newer disk contents.
        let source = file.source(side).to_string();
        let context = self.active_ws().and_then(|ws| {
            let (repo, relative) = path.split_once('/')?;
            Some(DocContext {
                repo: ws.dir().join(repo),
                rev: self.diff_revs.get(repo).cloned().filter(|r| !r.is_empty()),
                working_tree: side == DiffSide::Modified,
                dir: relative.rsplit_once('/').map_or("", |(d, _)| d).into(),
                web: self.prs.get(repo).and_then(|(pr, _)| {
                    pr.url.split_once("/pull/").map(|(url, _)| url.to_string())
                }),
            })
        });
        self.close_markdown(cx);
        for (_, view) in &self.media {
            view.update(cx, |v, cx| v.stop(cx));
        }
        self.diff_file = self
            .diff_files
            .iter()
            .position(|f| f.path() == path)
            .unwrap();
        self.follow_tree(self.diff_file);
        let cache = self.media_cache.clone();
        let doc = cx.new(|_| RichDoc::new(source, context, cache, true));
        self.markdown = Some(Preview {
            path: path.into(),
            side,
            doc,
        });
        cx.notify();
    }

    pub(super) fn close_markdown(&mut self, cx: &mut Context<Self>) {
        if let Some(preview) = self.markdown.take() {
            preview.doc.update(cx, |d, cx| d.stop(cx));
            cx.notify();
        }
    }

    pub(super) fn render_markdown(&self, cx: &mut Context<Self>) -> AnyElement {
        let preview = self.markdown.as_ref().unwrap();
        let file = self
            .diff_files
            .iter()
            .find(|f| f.path() == &preview.path)
            .unwrap();
        let side_button = |id: &'static str, label: &'static str, side, enabled: bool| {
            let path = preview.path.clone();
            Button::new(id)
                .label(label)
                .small()
                .ghost()
                .selected(preview.side == side)
                .disabled(!enabled)
                .on_click(cx.listener(move |s, _, _, cx| s.open_markdown(&path, side, cx)))
        };
        let doc = preview.doc.clone();
        v_flex()
            .id("diff-markdown-reader")
            .test_support()
            .key_context("DiffPane")
            .track_focus(&self.diff.read(cx).focus_handle(cx))
            .size_full()
            .min_h_0()
            .on_key_down(cx.listener(|s, e: &KeyDownEvent, _, cx| {
                match e.keystroke.key.as_str() {
                    "down" => s.scroll_markdown(40., cx),
                    "up" => s.scroll_markdown(-40., cx),
                    "pagedown" => s.scroll_markdown(600., cx),
                    "pageup" => s.scroll_markdown(-600., cx),
                    "home" => s.markdown.as_ref().unwrap().doc.read(cx).scroll_to_start(),
                    "end" => s.markdown.as_ref().unwrap().doc.read(cx).scroll_to_end(),
                    _ => return,
                }
                cx.notify();
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|s, _: &CancelEdit, w, cx| {
                s.close_markdown(cx);
                s.focus_diff(w, cx);
            }))
            .on_action(cx.listener(|s, _: &RefreshDiff, _, cx| s.refresh_diff(cx)))
            .on_action(cx.listener(|s, _: &SelectNext, _, cx| s.scroll_markdown(40., cx)))
            .on_action(cx.listener(|s, _: &SelectPrev, _, cx| s.scroll_markdown(-40., cx)))
            .on_action(cx.listener(|s, _: &PageDown, _, cx| s.scroll_markdown(600., cx)))
            .on_action(cx.listener(|s, _: &PageUp, _, cx| s.scroll_markdown(-600., cx)))
            .on_action(cx.listener(|s, _: &SelectFirst, _, cx| {
                s.markdown.as_ref().unwrap().doc.read(cx).scroll_to_start();
                cx.notify();
            }))
            .on_action(cx.listener(|s, _: &SelectLast, _, cx| {
                s.markdown.as_ref().unwrap().doc.read(cx).scroll_to_end();
                cx.notify();
            }))
            .child(
                h_flex()
                    .flex_none()
                    .gap_2()
                    .p_2()
                    .child(
                        Button::new("diff-markdown-back")
                            .label("Back to diff")
                            .small()
                            .ghost()
                            .on_click(cx.listener(|s, _, w, cx| {
                                s.close_markdown(cx);
                                s.focus_diff(w, cx);
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(preview.path.clone()),
                    )
                    .child(side_button(
                        "diff-markdown-before",
                        "Before",
                        DiffSide::Original,
                        file.status() != DiffFileStatus::Added,
                    ))
                    .child(side_button(
                        "diff-markdown-after",
                        "After",
                        DiffSide::Modified,
                        file.status() != DiffFileStatus::Deleted,
                    )),
            )
            .child(div().flex_1().min_h_0().child(doc))
            .into_any_element()
    }

    fn scroll_markdown(&self, delta: f32, cx: &mut Context<Self>) {
        if let Some(preview) = &self.markdown {
            preview.doc.read(cx).scroll_by(px(delta));
            cx.notify();
        }
    }
}

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use super::{Differ, Preferences};
    use crate::diff_view::{DiffFile, DiffLineRange, DiffSide};
    use gpui_kit::{AppContext, TestAppContext};

    #[gpui_kit::test]
    fn markdown_preview_uses_both_patch_sides_and_preserves_review_selection(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_kit::component::init(cx);
            crate::diff_view::init(cx);
        });
        let (differ, cx) = cx.add_window_view(|w, cx| {
            Differ::new(Preferences::from(&crate::state::State::default()), w, cx)
        });
        differ.update(cx, |d, cx| {
            let before = "# Before\n\nShared paragraph.\n";
            let after = "# After\n\nShared paragraph.\n";
            let patch = super::super::inline_edit::patch("repo/readme.MD", before, after, false);
            d.diff_files = DiffFile::parse(&patch).unwrap();
            d.diff_loaded = true;
            d.diff
                .update(cx, |s, cx| s.set_files(d.diff_files.clone(), cx));
            let selection = DiffLineRange::new("repo/readme.MD", DiffSide::Original, 1, 1);
            d.diff.update(cx, |s, cx| {
                s.set_selected_lines(Some(selection.clone()), cx)
            });
            for (side, expected) in [(DiffSide::Modified, after), (DiffSide::Original, before)] {
                d.open_markdown("repo/readme.MD", side, cx);
                let preview = d.markdown.as_ref().unwrap();
                assert_eq!(preview.doc.read(cx).source(), expected);
                assert_eq!(preview.side, side);
                assert!(preview.doc.read(cx).list().is_some());
            }
            d.close_markdown(cx);
            assert!(d.markdown.is_none());
            assert_eq!(d.diff.read(cx).selected_lines(), Some(selection));
            d.open_markdown("repo/readme.MD", DiffSide::Modified, cx);
            d.goto_file(0, cx);
            assert!(d.markdown.is_none(), "tree navigation leaves the preview");
        });
        for id in [
            "markdown-preview:repo/readme.MD",
            "diff-markdown-before",
            "diff-markdown-after",
            "diff-markdown-back",
        ] {
            cx.run_until_parked();
            let center = cx.update(|window, cx| {
                _ = window.draw(cx);
                gpui_kit::base::test_support::find_all(window, &[], &id.into())
                    .iter()
                    .find(|e| e.visible())
                    .expect("reader control is visible")
                    .bounds()
                    .center()
            });
            cx.simulate_click(center, gpui_kit::Modifiers::default());
            differ.update(cx, |d, _| match id {
                "markdown-preview:repo/readme.MD" => {
                    assert_eq!(d.markdown.as_ref().unwrap().side, DiffSide::Modified)
                }
                "diff-markdown-before" => {
                    assert_eq!(d.markdown.as_ref().unwrap().side, DiffSide::Original)
                }
                "diff-markdown-after" => {
                    assert_eq!(d.markdown.as_ref().unwrap().side, DiffSide::Modified)
                }
                _ => assert!(d.markdown.is_none()),
            });
        }
    }

    #[gpui_kit::test]
    fn markdown_preview_handles_added_deleted_and_non_markdown_files(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::component::init(cx);
            crate::diff_view::init(cx);
        });
        let (differ, cx) = cx.add_window_view(|w, cx| {
            Differ::new(Preferences::from(&crate::state::State::default()), w, cx)
        });
        differ.update(cx, |d, cx| {
            for (patch, expected_side, expected) in [
                ("diff --git a/repo/a.md b/repo/a.md\nnew file mode 100644\n--- /dev/null\n+++ b/repo/a.md\n@@ -0,0 +1 @@\n+# Added\n", DiffSide::Modified, "# Added\n"),
                ("diff --git a/repo/a.md b/repo/a.md\ndeleted file mode 100644\n--- a/repo/a.md\n+++ /dev/null\n@@ -1 +0,0 @@\n-# Deleted\n", DiffSide::Original, "# Deleted\n"),
            ] {
                d.diff_files = DiffFile::parse(patch).unwrap();
                d.open_markdown("repo/a.md", expected_side.other(), cx);
                let preview = d.markdown.as_ref().unwrap();
                assert_eq!(preview.side, expected_side);
                assert_eq!(preview.doc.read(cx).source(), expected);
                d.close_markdown(cx);
            }
            d.diff_files = vec![DiffFile::unchanged("repo/code.rs", "fn main() {}")];
            d.open_markdown("repo/code.rs", DiffSide::Modified, cx);
            assert!(d.markdown.is_none());
        });
    }
}
