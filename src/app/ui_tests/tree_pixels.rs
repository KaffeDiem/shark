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
            .child(self.0.update(cx, |s, cx| {
                s.differ
                    .update(cx, |d, cx| d.render_tree(w, cx).into_any_element())
            }))
    }
}

/// Full production layout, including Diff content, without constructing an AI chat.
pub(super) fn render_artifact_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let mut sync_menu_width = None;
    let mut sync_label_width = None;
    for width in [1440., 800.] {
        for (mode, label) in [
            (crate::state::Mode::Dark, "dark"),
            (crate::state::Mode::Light, "light"),
        ] {
            for rail in [true, false] {
                let mut state = State::default();
                state.appearance.mode = mode;
                state.tree_width = Some(200.);
                state.save().unwrap();
                let (window, shark) = cx
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
                            |w, cx| cx.new(|cx| Shark::new(w, cx)),
                        )
                    })
                    .unwrap();
                cx.update_window(window, |_, w, cx| {
                shark.update(cx, |s, cx| {
                    let mut ws = Workspace {
                        name: "Features and artifacts".into(),
                        branch: "feature/a-very-long-branch-name-that-must-truncate-before-the-diff-controls".into(),
                        repos: vec![crate::state::WsRepo {
                            name: "shark".into(),
                            src: home.join("shark"),
                            base: "main".into(),
                            branch: None,
                        }],
                        chats: vec![ChatMeta {
                            id: "layout-chat".into(),
                            ..Default::default()
                        }],
                        diff_closed: false,
                        ..Default::default()
                    };
                    ws.normalize_tabs();
                    let sync = cx.new(|cx| GitSync::new(ws.clone(), w, cx));
                    sync.update(cx, |sync, _| {
                        sync.set_merged_parent("shark", 1, "head", "main");
                        sync.action.as_mut().unwrap().label = "Commit and push · frontend, api".into();
                    });
                    s.syncs.insert(ws.name.clone(), sync);
                    s.rail = rail;
                    s.active = Some(ws.name.clone());
                    s.state.workspaces = vec![ws];
                    s.home.on = false;
                    s.tab = Tab::Feature(Feature::Diff);
                    s.sel = SideRow::Ws(0);
                    s.configure_differ(cx);
                    s.differ.update(cx, |d, cx| {
                    *d.show_tree_mut() = true;
                    *d.diff_files_mut() = DiffFile::parse(concat!(
                        "diff --git a/shark/src/main.rs b/shark/src/main.rs\n",
                        "--- a/shark/src/main.rs\n+++ b/shark/src/main.rs\n",
                        "@@ -1,3 +1,3 @@\n fn main() {\n",
                        "-    println!(\"Hello\");\n+    println!(\"Shark\");\n }\n",
                        "diff --git a/shark/README.md b/shark/README.md\n",
                        "--- a/shark/README.md\n+++ b/shark/README.md\n",
                        "@@ -1 +1 @@\n-Workspace tools\n+Features open artifact tabs.\n",
                    ))
                    .unwrap();
                    *d.diff_fps_mut() = vec![1; d.diff_files().len()];
                    *d.diff_loaded_mut() = true;
                    let files = d.diff_files().clone();
                    d.diff().update(cx, |state, cx| state.set_files(files, cx));
                    d.rebuild_tree();
                    d.diff().read(cx).focus_handle(cx).focus(w, cx);
                    });
                    assert!(s.chats.is_empty() && s.active_chat().is_none());
                    cx.notify();
                });
                w.pointer_move(point(px(width - 1.), px(719.)), None, cx);
                w.render_frame(cx);
            })
            .unwrap();
                for uncommitted in [false, true] {
                    cx.update_window(window, |_, _, cx| {
                        shark.update(cx, |s, cx| {
                            s.state.uncommitted_only = uncommitted;
                            s.state.unified = uncommitted;
                            s.differ.update(cx, |d, _| {
                                d.preferences_mut().uncommitted_only = uncommitted;
                                d.preferences_mut().unified = uncommitted;
                            });
                            s.differ.read(cx).diff().clone().update(cx, |d, cx| {
                                d.set_mode(
                                    if uncommitted {
                                        DiffMode::Unified
                                    } else {
                                        DiffMode::Split
                                    },
                                    cx,
                                )
                            });
                            cx.notify();
                        });
                    })
                    .unwrap();
                    cx.update_window(window, |_, w, cx| {
                        w.render_frame(cx);
                        let features = w.find("features-rail").bounds();
                        let island = w.find("content-island").bounds();
                        let files = w.find("file-tree").bounds();
                        assert_eq!(features.size.width, px(72.));
                        assert_eq!(features.right(), px(width));
                        assert!(w.find("feature-differ").visible());
                        assert!(w.find("diff-tab").visible());
                        assert!(w.find("file-tree").visible());
                        assert!(files.left() >= island.left() && files.right() <= island.right());
                        assert!(files.top() >= island.top() && files.bottom() <= island.bottom());
                        assert!(
                            files.left() - island.left() > px(100.),
                            "room for diff content"
                        );
                        assert!(island.right() <= features.left());
                        assert!(w.try_find("show-tree").is_none());
                        assert!(w.try_find("hide-tree").is_none());
                        let branch = w.find("branch").bounds();
                        let scope = w.find("uncommitted-only").bounds();
                        let layout = w.find("diff-mode").bounds();
                        let toggle = w.find("files-toggle").bounds();
                        let sync = w.find("git-next").bounds();
                        let menu = w.find("git-menu").bounds();
                        for id in [
                            "branch",
                            "uncommitted-only",
                            "diff-mode",
                            "files-toggle",
                            "git-next",
                            "git-menu",
                        ] {
                            let element = w.find(id);
                            let b = element.bounds();
                            assert!(element.visible(), "{label}, {width}, rail={rail}: {id}");
                            assert!(
                                b.left() >= island.left() && b.right() <= island.right(),
                                "{label}, {width}, rail={rail}: {id} {b:?} outside {island:?}"
                            );
                            assert_eq!(b.size.height, px(28.), "{id}");
                            assert_eq!(b.top(), scope.top(), "one row: {id}");
                        }
                        assert!(branch.size.width >= px(28.), "branch icon stays visible");
                        assert!(branch.right() <= scope.left());
                        assert_eq!(layout.size, size(px(28.), px(28.)));
                        assert_eq!(scope.right() + px(8.), layout.left());
                        assert_eq!(layout.right() + px(8.), toggle.left());
                        assert_eq!(toggle.right() + px(8.), sync.left());
                        assert_eq!(sync.right(), menu.left());
                        assert_eq!(
                            menu.size.width,
                            *sync_menu_width.get_or_insert(menu.size.width),
                            "Sync menu keeps its hit target at every width"
                        );
                        assert_eq!(
                            w.find("git-next").label(),
                            Some("Commit and push · frontend, api")
                        );
                        let full_label_width = *sync_label_width.get_or_insert(sync.size.width);
                        if width == 800. && !rail {
                            assert!(
                                sync.size.width < full_label_width,
                                "wide Sync label must truncate"
                            );
                        }
                        assert!(w.try_find("files-change-scope").is_none());
                        assert_eq!(
                            shark
                                .read(cx)
                                .differ
                                .read(cx)
                                .tree_resize()
                                .read(cx)
                                .sizes()[1],
                            px(200.)
                        );
                        assert!(shark.read(cx).active_chat().is_none());
                    })
                    .unwrap();
                    let screenshot = cx
                        .capture_screenshot(window)
                        .expect("full Diff artifact renderer");
                    if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
                        let dir = PathBuf::from(dir);
                        fs::create_dir_all(&dir).unwrap();
                        screenshot
                            .save(dir.join(format!(
                                "artifact-{label}-{}-rail-{rail}-uncommitted-{uncommitted}.png",
                                width as usize
                            )))
                            .unwrap();
                    }
                    cx.update_window(window, |_, w, cx| w.click("git-menu", cx))
                        .unwrap();
                    cx.update_window(window, |_, w, cx| {
                        w.render_frame(cx);
                        assert!(
                            w.find("menu-parent").visible(),
                            "Sync menu remains clickable"
                        );
                        w.press("escape", cx);
                    })
                    .unwrap();
                    cx.update_window(window, |_, w, cx| {
                        w.render_frame(cx);
                        assert!(w.try_find("menu-parent").is_none());
                    })
                    .unwrap();
                }
                // Review is an artifact overlay, so a narrow diff column cannot clip it.
                cx.update_window(window, |_, w, cx| {
                shark.update(cx, |s, cx| {
                    let comment = serde_json::from_str(r#"{"id":1,"path":"shark/src/main.rs","start":[false,2],"end":[false,2],"text":"Use the app name","status":"Pending"}"#).unwrap();
                    s.differ.update(cx, |d, cx| d.edit_comments(cx, |comments| comments.push(comment)));
                    s.differ.update(cx, |d, cx| d.open_sheet(w, cx));
                });
                w.render_frame(cx);
                let island = w.find("content-island").bounds();
                let sheet = w.find("review-sheet").bounds();
                let send = w.find("sheet-send").bounds();
                assert!(sheet.left() >= island.left() && sheet.right() <= island.right());
                assert!(send.left() >= sheet.left() && send.right() <= sheet.right());
            }).unwrap();
                cx.update_window(window, |_, w, _| w.remove_window())
                    .unwrap();
            }
        }
    }
    fs::remove_dir_all(&home).ok();
    println!(
        "Diff artifact: dark/light at 800/1440px, fixed Features rail and internal tree passed"
    );
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
                    s.differ.update(cx, |d, _| {
                    *d.diff_files_mut() = paths.iter().map(|p| {
                        DiffFile::parse(&format!("diff --git a/{p} b/{p}\n--- a/{p}\n+++ b/{p}\n@@ -1 +1 @@\n-old\n+new\n"))
                            .unwrap().remove(0)
                    }).collect();
                    *d.diff_fps_mut() = vec![1; paths.len()];
                    d.uncommitted_mut().insert(paths[0].clone());
                    });
                    if matches!(case, "viewed" | "changed") {
                        s.state.workspaces[0].viewed.insert(paths[0].clone(), if case == "viewed" { 1 } else { 2 });
                    }
                    if case == "open" {
                        let abs = home.join("sidebar.rs");
                        fs::create_dir_all(&home).unwrap();
                        fs::write(&abs, "new\n").unwrap();
                        s.editor = Some(cx.new(|cx| FileEditor::new(paths[0].clone(), abs, false, w, cx)));
                    }
                    s.differ.update(cx, |d, _cx| d.rebuild_tree());
                    s.configure_differ(cx);
                    s.differ.update(cx, |d, _| {
                    *d.tree_sel_mut() = if case == "compact-focus" {
                        d.tree_rows().iter().position(|r| r.path == paths[0]).unwrap()
                    } else { 1 };
                    });
                    if matches!(case, "focus" | "compact-focus") {
                        s.differ.read(cx).tree_focus().clone().focus(w, cx);
                    } else {
                        s.home.focus.focus(w, cx);
                    }
                    if case == "collapsed" {
                        s.differ.update(cx, |d, cx| d.set_dir_open(Some(false), cx));
                    } else if case == "filtered" {
                        s.differ.update(cx, |d, _| d.collapsed_dirs_mut().insert("api/src".into()));
                        s.differ.update(cx, |d, cx| d.filter_tree(".rs".into(), cx));
                    }
                    if matches!(case, "scroll" | "end") {
                        let target = if case == "end" { s.differ.read(cx).tree_rows().len() - 1 } else {
                            s.differ.read(cx).tree_rows().iter().position(|r| r.path.ends_with("f15.rs")).unwrap()
                        };
                        s.differ.read(cx).tree_scroll().scroll_to_item(target, ScrollStrategy::Top);
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
                    let viewport = s
                        .differ
                        .read(cx)
                        .tree_scroll()
                        .0
                        .borrow()
                        .base_handle
                        .bounds();
                    let mut checks = vec![];
                    let mut samples = vec![];
                    let mut visible = 0;
                    for (ix, row) in s.differ.read(cx).tree_rows().iter().enumerate() {
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
                        let focused = ix == s.differ.read(cx).tree_sel()
                            && s.differ.read(cx).tree_focus().is_focused(w);
                        let label = w.find(("tree-label", ix)).bounds();
                        assert!(
                            label.right() < b.right() - px(20.),
                            "label must leave room for the checkbox"
                        );
                        // Determine the endpoint from the folder's entire visible subtree, independently
                        // of the renderer's next-row calculation. Also sample both row boundaries.
                        for depth in 0..=row.depth {
                            let ancestor = (0..ix).rev().find(|&a| {
                                s.differ.read(cx).tree_rows()[a].depth == depth
                                    && s.differ.read(cx).tree_rows()[a].file.is_none()
                                    && row.path.starts_with(&format!(
                                        "{}/",
                                        s.differ.read(cx).tree_rows()[a].path
                                    ))
                            });
                            let last = ancestor.map(|a| {
                                let prefix = format!("{}/", s.differ.read(cx).tree_rows()[a].path);
                                s.differ
                                    .read(cx)
                                    .tree_rows()
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
                            let color = if s.differ.read(cx).viewed(file)
                                == crate::features::differ::Viewed::Yes
                            {
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
                    assert!(
                        shark
                            .read(cx)
                            .differ
                            .read(cx)
                            .collapsed_dirs()
                            .contains("api/src")
                    );
                    assert!(shark.read(cx).differ.read(cx).tree_focus().is_focused(w));
                    w.press("right", cx);
                    assert!(
                        !shark
                            .read(cx)
                            .differ
                            .read(cx)
                            .collapsed_dirs()
                            .contains("api/src")
                    );
                    w.click(("dir-viewed", 1usize), cx);
                    assert!(
                        !shark
                            .read(cx)
                            .differ
                            .read(cx)
                            .collapsed_dirs()
                            .contains("api/src"),
                        "checkbox must not fold the row"
                    );
                    assert_eq!(shark.read(cx).differ.read(cx).viewed_now().len(), 4);
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
