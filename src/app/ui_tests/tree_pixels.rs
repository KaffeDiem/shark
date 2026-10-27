//! Production file-tree geometry and colors through the real GPU renderer.
use super::*;
use gpui_kit::test::TestEventExt;

struct TreePreview(Entity<Shark>);

impl Render for TreePreview {
    fn render(&mut self, w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(cx.theme().background)
            .font_features(inter_features())
            .child(
                self.0
                    .update(cx, |s, cx| s.render_tree(w, cx).into_any_element()),
            )
    }
}

/// Full production layout, including Diff content, without constructing an AI chat.
pub(super) fn render_artifact_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    for (mode, label) in [
        (crate::state::Mode::Dark, "dark"),
        (crate::state::Mode::Light, "light"),
    ] {
        for width in [800., 1440.] {
            let mut state = State::default();
            state.appearance.mode = mode;
            state.save().unwrap();
            let (window, shark) = cx.update(|cx| {
                gpui_kit::open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            point(px(0.), px(0.)), size(px(width), px(720.)),
                        ))),
                        focus: false,
                        show: false,
                        ..Default::default()
                    },
                    cx,
                    |w, cx| cx.new(|cx| Shark::new(w, cx)),
                )
            }).unwrap();
            cx.update_window(window, |_, w, cx| {
                shark.update(cx, |s, cx| {
                    let mut ws = Workspace {
                        name: "Features and artifacts".into(),
                        branch: "ui-improv".into(),
                        repos: vec![crate::state::WsRepo {
                            name: "shark".into(), src: home.join("shark"),
                            base: "main".into(), branch: None,
                        }],
                        chats: vec![ChatMeta { id: "layout-chat".into(), ..Default::default() }],
                        diff_closed: false,
                        ..Default::default()
                    };
                    ws.normalize_tabs();
                    s.active = Some(ws.name.clone());
                    s.state.workspaces = vec![ws];
                    s.home.on = false;
                    s.tab = Tab::Diff;
                    s.sel = SideRow::Ws(0);
                    s.show_tree = true;
                    s.diff_files = DiffFile::parse(concat!(
                        "diff --git a/shark/src/main.rs b/shark/src/main.rs\n",
                        "--- a/shark/src/main.rs\n+++ b/shark/src/main.rs\n",
                        "@@ -1,3 +1,3 @@\n fn main() {\n",
                        "-    println!(\"Hello\");\n+    println!(\"Shark\");\n }\n",
                        "diff --git a/shark/README.md b/shark/README.md\n",
                        "--- a/shark/README.md\n+++ b/shark/README.md\n",
                        "@@ -1 +1 @@\n-Workspace tools\n+Features open artifact tabs.\n",
                    )).unwrap();
                    s.diff_fps = vec![1; s.diff_files.len()];
                    s.diff_loaded = true;
                    s.diff.update(cx, |d, cx| d.set_files(s.diff_files.clone(), cx));
                    s.rebuild_tree();
                    s.diff.read(cx).focus_handle(cx).focus(w, cx);
                    assert!(s.chats.is_empty() && s.active_chat().is_none());
                    cx.notify();
                });
                w.pointer_move(point(px(width - 1.), px(719.)), None, cx);
                w.render_frame(cx);
            }).unwrap();
            cx.update_window(window, |_, w, cx| {
                w.render_frame(cx);
                let rail = w.find("features-rail").bounds();
                let island = w.find("content-island").bounds();
                let files = w.find("file-tree").bounds();
                assert_eq!(rail.size.width, px(72.));
                assert_eq!(rail.right(), px(width));
                assert!(w.find("feature-differ").visible());
                assert!(w.find("diff-tab").visible());
                assert!(w.find("file-tree").visible());
                assert!(files.left() >= island.left() && files.right() <= island.right());
                assert!(files.top() >= island.top() && files.bottom() <= island.bottom());
                assert!(files.left() - island.left() > px(100.), "room for diff content");
                assert!(island.right() <= rail.left());
                assert!(w.try_find("show-tree").is_none());
                assert!(w.try_find("hide-tree").is_none());
                assert!(shark.read(cx).active_chat().is_none());
            }).unwrap();
            let screenshot = cx.capture_screenshot(window).expect("full Diff artifact renderer");
            if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
                let dir = PathBuf::from(dir);
                fs::create_dir_all(&dir).unwrap();
                screenshot.save(dir.join(format!("artifact-{label}-{}.png", width as usize))).unwrap();
            }
            // Review is an artifact overlay, so a narrow diff column cannot clip it.
            cx.update_window(window, |_, w, cx| {
                shark.update(cx, |s, cx| {
                    let comment = serde_json::from_str(r#"{"id":1,"path":"shark/src/main.rs","start":[false,2],"end":[false,2],"text":"Use the app name","status":"Pending"}"#).unwrap();
                    s.edit_comments(cx, |comments| comments.push(comment));
                    s.open_sheet(w, cx);
                });
                w.render_frame(cx);
                let island = w.find("content-island").bounds();
                let sheet = w.find("review-sheet").bounds();
                let send = w.find("sheet-send").bounds();
                assert!(sheet.left() >= island.left() && sheet.right() <= island.right());
                assert!(send.left() >= sheet.left() && send.right() <= sheet.right());
            }).unwrap();
            cx.update_window(window, |_, w, _| w.remove_window()).unwrap();
        }
    }
    fs::remove_dir_all(&home).ok();
    println!("Diff artifact: dark/light at 800/1440px, fixed Features rail and internal tree passed");
}

pub(super) fn render_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    cx.update(crate::bind_keys);
    for (mode, mode_name) in [
        (crate::state::Mode::Dark, "dark"),
        (crate::state::Mode::Light, "light"),
    ] {
        for case in [
            "normal",
            "focus",
            "hover",
            "compact-focus",
            "viewed",
            "changed",
            "open",
            "collapsed",
            "filtered",
            "narrow",
            "scroll",
            "end",
        ] {
            let mut state = State::default();
            state.appearance.mode = mode;
            state.save().unwrap();
            let width = if case == "narrow" { 200. } else { 400. };
            let (window, preview) = cx
                .update(|cx| {
                    gpui_kit::open_window(
                        WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                point(px(0.), px(0.)),
                                size(px(width), px(720.)),
                            ))),
                            focus: false,
                            show: false,
                            ..Default::default()
                        },
                        cx,
                        |w, cx| {
                            let shark = cx.new(|cx| Shark::new(w, cx));
                            cx.new(|_| TreePreview(shark))
                        },
                    )
                })
                .unwrap();
            let shark = cx.update(|cx| preview.read(cx).0.clone());
            cx.update_window(window, |_, w, cx| {
                shark.update(cx, |s, cx| {
                    s.state.workspaces = vec![Workspace { name: "tree".into(), ..Default::default() }];
                    s.active = Some("tree".into());
                    let mut paths: Vec<String> = [
                        "api/src/app/sidebar.rs", "api/src/deep/a.rs", "api/src/deep/b.rs",
                        "api/src/main.rs", "api/README.md", "web/index.ts", "ROOT.md",
                    ].map(String::from).to_vec();
                    paths.extend((0..40).map(|i| format!("zz/long/f{i:02}.rs")));
                    s.diff_files = paths.iter().map(|p| {
                        DiffFile::parse(&format!("diff --git a/{p} b/{p}\n--- a/{p}\n+++ b/{p}\n@@ -1 +1 @@\n-old\n+new\n"))
                            .unwrap().remove(0)
                    }).collect();
                    s.diff_fps = vec![1; paths.len()];
                    s.uncommitted.insert(paths[0].clone());
                    if matches!(case, "viewed" | "changed") {
                        s.state.workspaces[0].viewed.insert(paths[0].clone(), if case == "viewed" { 1 } else { 2 });
                    }
                    if case == "open" {
                        let abs = home.join("sidebar.rs");
                        fs::create_dir_all(&home).unwrap();
                        fs::write(&abs, "new\n").unwrap();
                        s.editor = Some(cx.new(|cx| FileEditor::new(paths[0].clone(), abs, false, w, cx)));
                    }
                    s.rebuild_tree();
                    s.tree_sel = if case == "compact-focus" {
                        s.tree_rows.iter().position(|r| r.path == paths[0]).unwrap()
                    } else { 1 };
                    if matches!(case, "focus" | "compact-focus") {
                        s.tree_focus.focus(w, cx);
                    } else {
                        s.home.focus.focus(w, cx);
                    }
                    if case == "collapsed" {
                        s.set_dir_open(Some(false), cx);
                    } else if case == "filtered" {
                        s.collapsed_dirs.insert("api/src".into());
                        s.filter_tree(".rs".into(), cx);
                    }
                    if matches!(case, "scroll" | "end") {
                        let target = if case == "end" { s.tree_rows.len() - 1 } else {
                            s.tree_rows.iter().position(|r| r.path.ends_with("f15.rs")).unwrap()
                        };
                        s.tree_scroll.scroll_to_item(target, ScrollStrategy::Top);
                    }
                    cx.notify();
                });
                w.pointer_move(point(px(width - 1.), px(719.)), None, cx);
                w.render_frame(cx);
                if case == "hover" {
                    w.hover(("tree-row", 2usize), cx);
                    w.render_frame(cx);
                }
            }).unwrap();
            let (checks, samples) = cx
                .update_window(window, |_, w, cx| {
                    let s = shark.read(cx);
                    let theme = cx.theme();
                    let viewport = s.tree_scroll.0.borrow().base_handle.bounds();
                    let mut checks = vec![];
                    let mut samples = vec![];
                    let mut visible = 0;
                    for (ix, row) in s.tree_rows.iter().enumerate() {
                        let Some(snapshot) = w.try_find(("tree-row", ix)) else {
                            continue;
                        };
                        let b = snapshot.bounds();
                        if !snapshot.visible()
                            || b.top() < viewport.top()
                            || b.bottom() > viewport.bottom()
                        {
                            continue;
                        }
                        visible += 1;
                        assert_eq!(b.size.height, px(30.));
                        let focused = ix == s.tree_sel && s.tree_focus.is_focused(w);
                        let label = w.find(("tree-label", ix)).bounds();
                        assert!(
                            label.right() < b.right() - px(20.),
                            "label must leave room for the checkbox"
                        );
                        // Determine the endpoint from the folder's entire visible subtree, independently
                        // of the renderer's next-row calculation. Also sample both row boundaries.
                        for depth in 0..=row.depth {
                            let ancestor = (0..ix).rev().find(|&a| {
                                s.tree_rows[a].depth == depth
                                    && s.tree_rows[a].file.is_none()
                                    && row.path.starts_with(&format!("{}/", s.tree_rows[a].path))
                            });
                            let last = ancestor.map(|a| {
                                let prefix = format!("{}/", s.tree_rows[a].path);
                                s.tree_rows
                                    .iter()
                                    .rposition(|r| r.path.starts_with(&prefix))
                                    .unwrap()
                            });
                            for y in [0., 2., 14., 15., 27., 29.] {
                                // The current row's icon occupies its own column, not an ancestor guide.
                                if depth == row.depth && (2. ..28.).contains(&y) {
                                    continue;
                                }
                                samples.push((
                                    format!("row {ix}, depth {depth}, y {y}"),
                                    point(b.left() + px(16. + 12. * depth as f32), b.top() + px(y)),
                                    last.is_some_and(|last| last > ix || y < 15.)
                                        && !(focused && (y == 0. || y == 29.)),
                                    theme.border,
                                ));
                            }
                        }
                        if row.file.is_none() {
                            let color = theme.foreground;
                            checks.push(("folder label", label, color));
                            checks.push((
                                "folder chevron",
                                Bounds::new(
                                    point(
                                        b.left() + px(8. + 12. * row.depth as f32),
                                        b.top() + px(7.),
                                    ),
                                    size(px(16.), px(16.)),
                                ),
                                color,
                            ));
                        } else if case != "narrow" {
                            let file = row.file.unwrap();
                            let color = if s.viewed(file) == super::super::diff::Viewed::Yes {
                                theme.muted_foreground
                            } else {
                                theme.foreground
                            };
                            if row.name.starts_with("app/") {
                                checks.push((
                                    "compact prefix",
                                    Bounds::new(label.origin, size(px(24.), label.size.height)),
                                    theme.foreground,
                                ));
                                checks.push((
                                    "filename",
                                    Bounds::new(
                                        point(label.left() + px(32.), label.top()),
                                        size(label.size.width - px(32.), label.size.height),
                                    ),
                                    color,
                                ));
                                checks.push((
                                    "uncommitted dot",
                                    w.find(("uncommitted", ix)).bounds(),
                                    theme.primary,
                                ));
                                if case == "changed" {
                                    checks.push((
                                        "changed dot",
                                        Bounds::new(
                                            point(label.right(), b.top()),
                                            size(
                                                b.right() - label.right() - px(24.),
                                                b.size.height,
                                            ),
                                        ),
                                        theme.warning_foreground,
                                    ));
                                }
                            } else {
                                checks.push(("filename", label, color));
                            }
                            checks.push(("additions", b, theme.success_foreground));
                            checks.push(("deletions", b, theme.danger_foreground));
                        }
                        if focused {
                            checks.push((
                                "focus ring",
                                Bounds::new(b.origin, size(b.size.width, px(1.))),
                                theme.primary,
                            ));
                        }
                    }
                    assert!(visible > 0);
                    if matches!(case, "scroll" | "end") {
                        assert!(
                            w.try_find(("tree-row", 0usize)).is_none(),
                            "scroll must virtualize the ancestor away"
                        );
                    }
                    (checks, samples)
                })
                .unwrap();
            let screenshot = cx.capture_screenshot(window).expect("real tree renderer");
            if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
                let dir = PathBuf::from(dir);
                fs::create_dir_all(&dir).unwrap();
                screenshot
                    .save(dir.join(format!("tree-{mode_name}-{case}.png")))
                    .unwrap();
            }
            let scale = screenshot.width() as f32 / width;
            let matches = |x: u32, y: u32, color: Hsla, tolerance: u8| {
                let rgb = color.to_rgb();
                let expected = [rgb.r, rgb.g, rgb.b].map(|c| (c * 255.).round() as u8);
                let actual = screenshot.get_pixel(x, y);
                (0..3).all(|c| actual[c].abs_diff(expected[c]) <= tolerance)
            };
            for (label, point, present, color) in samples {
                assert_eq!(
                    matches(
                        (point.x.as_f32() * scale).floor() as u32,
                        (point.y.as_f32() * scale).floor() as u32,
                        color,
                        0
                    ),
                    present,
                    "{mode_name} {case}: guide {label}"
                );
            }
            for (label, b, color) in checks {
                let mut count = 0;
                for y in (b.top().as_f32() * scale).ceil() as u32
                    ..(b.bottom().as_f32() * scale).floor() as u32
                {
                    for x in (b.left().as_f32() * scale).ceil() as u32
                        ..(b.right().as_f32() * scale).floor() as u32
                    {
                        count += usize::from(matches(x, y, color, 4));
                    }
                }
                assert!(
                    count > 2,
                    "{mode_name} {case}: {label} must render {color:?} in {b:?}, got {count} pixels"
                );
            }
            // A click on a painted ancestor guide still reaches the nested folder row.
            if case == "normal" {
                cx.update_window(window, |_, w, cx| {
                    w.click_at(("tree-row", 1usize), point(px(16.), px(10.)), cx);
                    assert!(shark.read(cx).collapsed_dirs.contains("api/src"));
                    assert!(shark.read(cx).tree_focus.is_focused(w));
                    w.press("right", cx);
                    assert!(!shark.read(cx).collapsed_dirs.contains("api/src"));
                    w.click(("dir-viewed", 1usize), cx);
                    assert!(
                        !shark.read(cx).collapsed_dirs.contains("api/src"),
                        "checkbox must not fold the row"
                    );
                    assert_eq!(shark.read(cx).viewed_now.len(), 4);
                })
                .unwrap();
            }
            cx.update_window(window, |_, w, _| w.remove_window())
                .unwrap();
        }
    }
    fs::remove_dir_all(&home).ok();
    println!(
        "tree pixels: dark/light guides, compaction, focus, statuses, collapse, filtering, scrolling and narrow labels passed"
    );
}
