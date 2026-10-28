//! The language edit path uses real GPUI buffers and the normal mixed-tab workspace.
use super::*;
use crate::lsp::document::{EditGuard, Encoding};
use core::prelude::v1::test;
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use lsp_types::{Position, Range, TextEdit, WorkspaceEdit};
use std::time::Duration;

fn file_uri(path: &Path) -> lsp_types::Uri {
    url::Url::from_file_path(path)
        .unwrap()
        .as_str()
        .parse()
        .unwrap()
}

fn edits(root: &Path, first: &str, second: &str) -> WorkspaceEdit {
    let edit = |text: &str| {
        vec![TextEdit::new(
            Range::new(Position::new(0, 0), Position::new(0, 5)),
            text.into(),
        )]
    };
    WorkspaceEdit {
        changes: Some(HashMap::from([
            (file_uri(&root.join("a.txt")), edit(first)),
            (file_uri(&root.join("b.txt")), edit(second)),
        ])),
        ..Default::default()
    }
}

async fn fixture(cx: &mut TestAppContext) -> (PathBuf, AnyWindowHandle, Entity<Shark>) {
    let home = crate::state::TEST_HOME
        .get_or_init(|| std::env::temp_dir().join(format!("shark-ui-{}", std::process::id())))
        .clone();
    let mut ws = Workspace {
        name: "language-proof".into(),
        branch: "main".into(),
        repos: vec![crate::state::WsRepo {
            name: "api".into(),
            src: home.join("lsp-source"),
            base: "main".into(),
            branch: None,
        }],
        ..Default::default()
    };
    ws.migrate();
    let root = ws.dir().join("api");
    let _ = std::fs::remove_dir_all(ws.dir());
    std::fs::create_dir_all(&root).unwrap();
    git::git(&root, &["init", "-q", "-b", "main"]).unwrap();
    git::git(
        &root,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@test",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ],
    )
    .unwrap();
    std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
    std::fs::write(root.join("b.txt"), "bravo\n").unwrap();
    ws.open_tab(TabId::File("api/a.txt".into()));
    ws.diff_closed = false;
    ws.open_tab(TabId::Diff);
    ws.open_tab(TabId::File("api/b.txt".into()));
    let state = State {
        workspaces: vec![ws],
        last_open: Some(("language-proof".into(), "chat".into())),
        ..Default::default()
    };
    state.save().unwrap();
    let (window, shark) = cx.update(|cx| {
        gpui_kit::init(cx);
        crate::bind_keys(cx);
        crate::menubar::init(cx);
        gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
            cx.new(|cx| Shark::new(w, cx))
        })
        .unwrap()
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).file_tabs.len() == 2
    })
    .await;
    (root, window, shark)
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn language_outline_and_history_shortcuts_reach_the_focused_editor(cx: &mut TestAppContext) {
    let _guard = ui_tests::UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, window, shark) = fixture(cx).await;
    let editor = cx
        .update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| {
                s.select_file(0, w, cx);
                let editor = s.editor.clone().unwrap();
                editor.update(cx, |e, cx| {
                    e.outline = vec![crate::editor::Symbol {
                        name: "alpha".into(),
                        depth: 0,
                        range: 0..5,
                        target: 3,
                    }];
                    e.input.update(cx, |i, cx| {
                        i.set_selected_range(0..0, cx);
                        i.focus(w, cx);
                    });
                });
                editor
            })
        })
        .unwrap();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(editor.read(cx).is_focused(w, cx));
        w.press("cmd-shift-o", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.has_active_dialog(cx));
        // The outline refreshes while the menu is open; its row still means `alpha`.
        editor.update(cx, |e, _| {
            e.outline.insert(
                0,
                crate::editor::Symbol {
                    name: "inserted".into(),
                    depth: 0,
                    range: 0..1,
                    target: 1,
                },
            )
        });
        w.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        assert_eq!(
            editor.read(cx).input.read(cx).selected_range(),
            3..3,
            "Cmd-Shift-O must select a document symbol, not open the workspace picker"
        );
        assert!(editor.read(cx).is_focused(w, cx));
        w.render_frame(cx);
        w.press("ctrl--", cx);
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        editor.read(cx).input.read(cx).selected_range() == (0..0)
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        assert!(editor.read(cx).is_focused(w, cx));
        w.render_frame(cx);
        w.press("ctrl-_", cx);
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        editor.read(cx).input.read(cx).selected_range() == (3..3)
    })
    .await;
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn language_batches_are_atomic_dirty_undoable_and_preserve_tabs(cx: &mut TestAppContext) {
    let _guard = ui_tests::UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (root, window, shark) = fixture(cx).await;
    let tab_order = cx.update(|cx| shark.read(cx).active_ws().unwrap().tab_order.clone());
    let buffers = cx.update(|cx| {
        shark
            .read(cx)
            .file_tabs
            .iter()
            .map(|t| t.editor.clone())
            .collect::<Vec<_>>()
    });
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.set_vim(false, cx);
            s.apply_workspace_edit(
                edits(&root, "one😀", "twoé"),
                Encoding::Utf16,
                root.clone(),
                Some(EditGuard::capture()),
                None,
                w,
                cx,
            );
        })
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        buffers.iter().all(|e| e.read(cx).dirty)
    })
    .await;
    cx.update(|cx| {
        assert_eq!(
            buffers[0].read(cx).input.read(cx).value().as_ref(),
            "one😀\n"
        );
        assert_eq!(
            buffers[1].read(cx).input.read(cx).value().as_ref(),
            "twoé\n"
        );
        assert_eq!(shark.read(cx).active_ws().unwrap().tab_order, tab_order);
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "alpha\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("b.txt")).unwrap(),
            "bravo\n"
        );
    });
    // Each document's batch is one undo step, independent of the other tab.
    for (index, expected) in [(0, "alpha\n"), (1, "bravo\n")] {
        cx.update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| s.select_file(index, w, cx));
            w.render_frame(cx);
            w.press("cmd-z", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(
                buffers[index].read(cx).input.read(cx).value().as_ref(),
                expected
            )
        });
    }
    // A valid first target cannot change when another target has a save conflict.
    buffers[1].update(cx, |e, _| e.conflict = true);
    let before = cx
        .update_window(window, |_, w, cx| w.notifications(cx).len())
        .unwrap();
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.apply_workspace_edit(
                edits(&root, "first", "other"),
                Encoding::Utf16,
                root.clone(),
                Some(EditGuard::capture()),
                None,
                w,
                cx,
            )
        })
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |w, cx| {
        w.notifications(cx).len() > before
    })
    .await;
    cx.update(|cx| {
        assert_eq!(
            buffers[0].read(cx).input.read(cx).value().as_ref(),
            "alpha\n"
        );
        assert_eq!(
            buffers[1].read(cx).input.read(cx).value().as_ref(),
            "bravo\n"
        );
    });
    buffers[1].update(cx, |e, _| e.conflict = false);
    for invalid in [
        serde_json::json!({"documentChanges":[{"textDocument":{"uri":file_uri(&root.join("a.txt")),"version":999},"edits":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":5}},"newText":"stale"}]}]}),
        serde_json::json!({"documentChanges":[{"textDocument":{"uri":file_uri(&root.join("a.txt")),"version":null},"edits":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":5}},"newText":"first"}]},{"kind":"delete","uri":file_uri(&root.join("b.txt"))}]}),
    ] {
        let before = cx
            .update_window(window, |_, w, cx| w.notifications(cx).len())
            .unwrap();
        let edit = serde_json::from_value(invalid).unwrap();
        cx.update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| {
                s.apply_workspace_edit(
                    edit,
                    Encoding::Utf16,
                    root.clone(),
                    Some(EditGuard::capture()),
                    None,
                    w,
                    cx,
                )
            })
        })
        .unwrap();
        cx.wait_for(window, Duration::from_secs(10), |w, cx| {
            w.notifications(cx).len() > before
        })
        .await;
        cx.update(|cx| {
            assert_eq!(
                buffers[0].read(cx).input.read(cx).value().as_ref(),
                "alpha\n"
            )
        });
        assert!(root.join("b.txt").is_file());
    }
    // An unversioned response cannot reinterpret its old ranges after another open buffer changed.
    let guard = EditGuard::capture();
    cx.update_window(window, |_, w, cx| {
        buffers[0].update(cx, |e, cx| e.replace_language_text("newer\n".into(), w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    assert!(!guard.is_current());
    let before = cx
        .update_window(window, |_, w, cx| w.notifications(cx).len())
        .unwrap();
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.apply_workspace_edit(
                edits(&root, "wrong", "wrong"),
                Encoding::Utf16,
                root.clone(),
                Some(guard),
                None,
                w,
                cx,
            )
        })
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |w, cx| {
        w.notifications(cx).len() > before
    })
    .await;
    cx.update(|cx| {
        assert_eq!(
            buffers[0].read(cx).input.read(cx).value().as_ref(),
            "newer\n"
        );
        assert_eq!(
            buffers[1].read(cx).input.read(cx).value().as_ref(),
            "bravo\n"
        );
    });
    // Closed files written since the request are rejected before any open target changes.
    let mut guard = EditGuard::capture();
    guard.started -= Duration::from_secs(1);
    std::fs::write(root.join("c.txt"), "agent\n").unwrap();
    let mut edit = edits(&root, "wrong", "wrong");
    edit.changes.as_mut().unwrap().insert(
        file_uri(&root.join("c.txt")),
        vec![TextEdit::new(
            Range::new(Position::new(0, 0), Position::new(0, 5)),
            "wrong".into(),
        )],
    );
    let before = cx
        .update_window(window, |_, w, cx| w.notifications(cx).len())
        .unwrap();
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.apply_workspace_edit(
                edit,
                Encoding::Utf16,
                root.clone(),
                Some(guard),
                None,
                w,
                cx,
            )
        })
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |w, cx| {
        w.notifications(cx).len() > before
    })
    .await;
    assert_eq!(
        std::fs::read_to_string(root.join("c.txt")).unwrap(),
        "agent\n"
    );
    cx.update(|cx| {
        assert_eq!(shark.read(cx).file_tabs.len(), 2);
        assert_eq!(
            buffers[0].read(cx).input.read(cx).value().as_ref(),
            "newer\n"
        );
    });
    // A dirty diff buffer participates in the same preflight as full-width editor tabs.
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            buffers[1].update(cx, |e, _| e.dirty = false);
            s.show(Tab::Feature(Feature::Diff), w, cx);
            s.refresh_diff(cx);
        })
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark
            .read(cx)
            .differ
            .read(cx)
            .diff_files()
            .iter()
            .any(|f| f.path().as_str() == "api/b.txt")
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.edit_working_copy(Some("api/b.txt".into()), w, cx)
        })
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).differ.read(cx).editing().is_some()
    })
    .await;
    cx.update(|cx| {
        shark
            .read(cx)
            .differ
            .clone()
            .update(cx, |d, _| d.editing_mut().as_mut().unwrap().dirty = true)
    });
    let before = cx
        .update_window(window, |_, w, cx| w.notifications(cx).len())
        .unwrap();
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.apply_workspace_edit(
                edits(&root, "wrong", "wrong"),
                Encoding::Utf16,
                root.clone(),
                Some(EditGuard::capture()),
                None,
                w,
                cx,
            )
        })
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |w, cx| {
        w.notifications(cx).len() > before
    })
    .await;
    cx.update(|cx| {
        assert_eq!(
            buffers[0].read(cx).input.read(cx).value().as_ref(),
            "newer\n"
        );
        assert_eq!(
            buffers[1].read(cx).input.read(cx).value().as_ref(),
            "bravo\n"
        );
        assert!(
            shark
                .read(cx)
                .differ
                .read(cx)
                .editing()
                .as_ref()
                .unwrap()
                .dirty
        );
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn language_navigation_reuses_aliases_restores_history_and_protects_external_sources(
    cx: &mut TestAppContext,
) {
    let _guard = ui_tests::UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (root, window, shark) = fixture(cx).await;
    let link = |path: &Path, line: u32, start: u32, end: u32| {
        let range = Range::new(Position::new(line, start), Position::new(line, end));
        lsp_types::LocationLink {
            origin_selection_range: None,
            target_uri: file_uri(path),
            target_range: range,
            target_selection_range: range,
        }
    };
    let buffers = cx.update(|cx| {
        shark
            .read(cx)
            .file_tabs
            .iter()
            .map(|t| t.editor.clone())
            .collect::<Vec<_>>()
    });
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.set_vim(false, cx);
            s.select_file(1, w, cx);
            buffers[1]
                .read(cx)
                .input
                .clone()
                .update(cx, |i, cx| i.set_selected_range(1..4, cx));
        })
    })
    .unwrap();
    std::os::unix::fs::symlink(root.join("a.txt"), root.join("alias.txt")).unwrap();
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.open_location(
                link(&root.join("alias.txt"), 0, 1, 3),
                Encoding::Utf16,
                true,
                false,
                w,
                cx,
            )
        })
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).editor.as_ref() == Some(&buffers[0])
    })
    .await;
    cx.update(|cx| {
        assert_eq!(
            shark.read(cx).file_tabs.len(),
            2,
            "a symlink location must reuse its open source tab"
        );
        assert_eq!(buffers[0].read(cx).input.read(cx).selected_range(), 1..3);
    });
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.navigate_history(false, w, cx))
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).editor.as_ref() == Some(&buffers[1])
    })
    .await;
    cx.update(|cx| assert_eq!(buffers[1].read(cx).input.read(cx).selected_range(), 1..4));
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.navigate_history(true, w, cx))
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).editor.as_ref() == Some(&buffers[0])
    })
    .await;
    cx.update(|cx| assert_eq!(buffers[0].read(cx).input.read(cx).selected_range(), 1..3));

    // Multiple definitions must expose the chooser; selecting its second result opens that file.
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.pick_locations(
                vec![
                    link(&root.join("a.txt"), 0, 0, 1),
                    link(&root.join("b.txt"), 0, 0, 2),
                ],
                Encoding::Utf16,
                false,
                w,
                cx,
            )
        })
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.press("down", cx);
        w.press("enter", cx);
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).editor.as_ref() == Some(&buffers[1])
    })
    .await;
    cx.update(|cx| assert_eq!(buffers[1].read(cx).input.read(cx).selected_range(), 0..2));

    let external = root
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("language-external.rs");
    let contents = (0..200)
        .map(|_| "😀 dependency source\n")
        .collect::<String>();
    std::fs::write(&external, &contents).unwrap();
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.open_location(
                link(&external, 50, 3, 13),
                Encoding::Utf16,
                true,
                true,
                w,
                cx,
            )
        })
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark
            .read(cx)
            .editor
            .as_ref()
            .is_some_and(|e| e.read(cx).external)
    })
    .await;
    let dependency = cx.update(|cx| shark.read(cx).editor.as_ref().unwrap().clone());
    let selection = cx.update(|cx| dependency.read(cx).input.read(cx).selected_range());
    assert_eq!(&contents[selection.clone()], "dependency");
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.input("must not change", cx);
        dependency.read(cx).input.clone().update(cx, |i, cx| {
            i.set_scroll_offset(point(px(0.), px(-200.)), cx)
        });
    })
    .unwrap();
    cx.run_until_parked();
    let scroll = cx.update(|cx| dependency.read(cx).input.read(cx).scroll_offset());
    cx.update(|cx| {
        assert_eq!(
            dependency.read(cx).input.read(cx).value().as_ref(),
            contents
        );
        assert!(!dependency.read(cx).dirty);
    });
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.open_location(
                link(&root.join("a.txt"), 0, 0, 1),
                Encoding::Utf16,
                true,
                false,
                w,
                cx,
            )
        })
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).editor.as_ref() == Some(&buffers[0])
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.navigate_history(false, w, cx))
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).editor.as_ref() == Some(&dependency)
    })
    .await;
    cx.update(|cx| {
        assert_eq!(
            dependency.read(cx).input.read(cx).selected_range(),
            selection
        );
        assert_eq!(dependency.read(cx).input.read(cx).scroll_offset(), scroll);
        assert_eq!(shark.read(cx).file_tabs.len(), 3);
    });
}

// cmd-click follows a definition in the current tab; cmd-shift-click (new_tab) keeps it.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn language_navigation_replaces_a_clean_tab_unless_asked_for_a_new_one(
    cx: &mut TestAppContext,
) {
    let _guard = ui_tests::UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (root, window, shark) = fixture(cx).await;
    std::fs::write(root.join("c.txt"), "charlie\n").unwrap();
    std::fs::write(root.join("d.txt"), "delta\n").unwrap();
    let link = |name: &str| {
        let range = Range::new(Position::new(0, 1), Position::new(0, 3));
        lsp_types::LocationLink {
            origin_selection_range: None,
            target_uri: file_uri(&root.join(name)),
            target_range: range,
            target_selection_range: range,
        }
    };
    let paths = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let s = shark.read(cx);
            let tabs: Vec<_> = (s.file_tabs.iter())
                .map(|t| t.editor.read(cx).path.clone())
                .collect();
            (tabs, s.editor.as_ref().unwrap().read(cx).path.clone())
        })
    };
    let open = |cx: &mut TestAppContext, name: &str, new_tab: bool| {
        let link = link(name);
        cx.update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| {
                s.open_location(link, Encoding::Utf16, true, new_tab, w, cx)
            })
        })
        .unwrap();
    };
    let wait_for = async |cx: &mut TestAppContext, path: &'static str| {
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            shark
                .read(cx)
                .editor
                .as_ref()
                .is_some_and(|e| e.read(cx).path == path)
        })
        .await;
    };
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.select_file(0, w, cx))
    })
    .unwrap();
    open(cx, "c.txt", false);
    wait_for(cx, "api/c.txt").await;
    assert_eq!(
        paths(cx).0,
        ["api/c.txt", "api/b.txt"],
        "a.txt's tab now shows c.txt"
    );
    open(cx, "d.txt", true);
    wait_for(cx, "api/d.txt").await;
    assert_eq!(paths(cx).0, ["api/c.txt", "api/b.txt", "api/d.txt"]);
    // Back from a new tab selects the tab it came from; back again restores a.txt in place.
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.navigate_history(false, w, cx))
    })
    .unwrap();
    wait_for(cx, "api/c.txt").await;
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.navigate_history(false, w, cx))
    })
    .unwrap();
    wait_for(cx, "api/a.txt").await;
    assert_eq!(paths(cx).0, ["api/a.txt", "api/b.txt", "api/d.txt"]);
    // Unsaved edits are never replaced away.
    cx.update(|cx| {
        let editor = shark.read(cx).editor.clone().unwrap();
        editor.update(cx, |e, _| e.dirty = true);
    });
    open(cx, "c.txt", false);
    wait_for(cx, "api/c.txt").await;
    assert_eq!(
        paths(cx).0,
        ["api/a.txt", "api/b.txt", "api/d.txt", "api/c.txt"]
    );
    cx.update(|cx| {
        let tabs = &shark.read(cx).active_ws().unwrap().tab_order;
        assert_eq!(
            tabs.iter().filter(|t| matches!(t, TabId::File(_))).count(),
            4,
            "the saved tab order follows the replacement"
        );
    });
}

// Vim's Normal mode: `g s` opens the outline at the cursor's symbol, `ctrl-o`/`ctrl-i` walk
// the jump list.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn vim_keys_open_the_outline_and_walk_the_jump_list(cx: &mut TestAppContext) {
    let _guard = ui_tests::UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, window, shark) = fixture(cx).await;
    let editor = cx.update(|cx| shark.read(cx).file_tabs[0].editor.clone());
    let symbol = |name: &str, depth, range: std::ops::Range<usize>| crate::editor::Symbol {
        name: name.into(),
        depth,
        target: range.start,
        range,
    };
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.set_vim(true, cx);
            s.select_file(0, w, cx);
        });
        editor.update(cx, |e, cx| {
            // "alpha\n": a file symbol around a nested one.
            e.outline = vec![symbol("file", 0, 0..6), symbol("ph", 1, 2..4)];
            e.input.update(cx, |i, cx| {
                i.set_selected_range(0..0, cx);
                i.focus(w, cx);
            });
            e.sync_crumbs(cx);
        });
        w.render_frame(cx);
        w.press("g", cx);
        w.press("s", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.press("down", cx);
        w.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let cursor = |cx: &mut TestAppContext| cx.update(|cx| editor.read(cx).input.read(cx).cursor());
    assert_eq!(
        cursor(cx),
        2,
        "the outline's second row is the nested symbol"
    );
    cx.update(|cx| assert_eq!(editor.read(cx).crumbs, [0, 1]));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.press("ctrl-o", cx);
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        editor.read(cx).input.read(cx).cursor() == 0
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.press("ctrl-i", cx);
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        editor.read(cx).input.read(cx).cursor() == 2
    })
    .await;
}

// A folder in the jump bar lists its entries, folders first, starting at the open file;
// picking a file opens it in place of the current one.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn jump_bar_folder_lists_its_files(cx: &mut TestAppContext) {
    let _guard = ui_tests::UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (root, window, shark) = fixture(cx).await;
    std::fs::create_dir(root.join("sub")).unwrap();
    std::fs::write(root.join("c.txt"), "charlie\n").unwrap();
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.set_vim(false, cx);
            s.select_file(0, w, cx);
            s.folder_menu("api".into(), w, cx);
        })
    })
    .unwrap();
    cx.run_until_parked();
    // sub/, a.txt (selected), b.txt, c.txt
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.press("down", cx);
        w.press("down", cx);
        w.press("enter", cx);
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark
            .read(cx)
            .editor
            .as_ref()
            .is_some_and(|e| e.read(cx).path == "api/c.txt")
    })
    .await;
    cx.update(|cx| {
        let tabs: Vec<_> = (shark.read(cx).file_tabs.iter())
            .map(|t| t.editor.read(cx).path.clone())
            .collect();
        assert_eq!(tabs, ["api/c.txt", "api/b.txt"]);
        assert_eq!(
            shark.read(cx).navigation.back.len(),
            1,
            "Back returns to a.txt"
        );
    });
}

// Code renders at 14px on 21px rows by default; cmd-=/cmd-- resize every open file and
// persist, within the readable range.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn editor_text_size_defaults_to_research_values_and_steps(cx: &mut TestAppContext) {
    let _guard = ui_tests::UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, window, shark) = fixture(cx).await;
    let editors = cx.update(|cx| {
        (shark.read(cx).file_tabs.iter())
            .map(|t| t.editor.clone())
            .collect::<Vec<_>>()
    });
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.select_file(0, w, cx));
        editors[0].update(cx, |e, cx| e.input.update(cx, |i, cx| i.focus(w, cx)));
        w.render_frame(cx);
    })
    .unwrap();
    let row =
        |cx: &mut TestAppContext| cx.update(|cx| editors[0].read(cx).input.read(cx).line_height());
    assert_eq!(row(cx), Some(px(21.)));
    cx.update_window(window, |_, w, cx| {
        w.press("cmd-=", cx);
        w.press("cmd-=", cx);
    })
    .unwrap();
    cx.update(|cx| {
        assert_eq!(crate::editor::code_font(cx), (px(16.), px(24.)));
        assert_eq!(shark.read(cx).state.editor_font_size, Some(16.));
    });
    assert_eq!(row(cx), Some(px(24.)));
    cx.update_window(window, |_, w, cx| {
        for _ in 0..20 {
            w.press("cmd--", cx);
        }
    })
    .unwrap();
    cx.update(|cx| assert_eq!(crate::editor::code_font(cx).0, px(10.)));
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.step_editor_font(0., w, cx))
    })
    .unwrap();
    cx.update(|cx| {
        assert_eq!(crate::editor::code_font(cx), (px(14.), px(21.)));
        assert_eq!(shark.read(cx).state.editor_font_size, None);
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn language_navigation_failures_preserve_history_and_late_results_preserve_focus(
    cx: &mut TestAppContext,
) {
    let _guard = ui_tests::UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (root, window, shark) = fixture(cx).await;
    std::fs::write(root.join("c.txt"), "charlie\n").unwrap();
    let link = || {
        let range = Range::new(Position::new(0, 0), Position::new(0, 1));
        lsp_types::LocationLink {
            origin_selection_range: None,
            target_uri: file_uri(&root.join("c.txt")),
            target_range: range,
            target_selection_range: range,
        }
    };
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.select_file(0, w, cx);
            s.open_location(link(), Encoding::Utf16, true, false, w, cx);
        })
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).editor.as_ref().unwrap().read(cx).path == "api/c.txt"
    })
    .await;
    // An unreadable (not deleted) file may be fixed; Back keeps trying it.
    std::fs::write(root.join("a.txt"), "alpha\0\n").unwrap();
    let before = cx
        .update_window(window, |_, w, cx| w.notifications(cx).len())
        .unwrap();
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.navigate_history(false, w, cx))
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |w, cx| {
        w.notifications(cx).len() > before
    })
    .await;
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.editor.as_ref().unwrap().read(cx).path, "api/c.txt");
        assert_eq!(
            s.navigation.back.len(),
            1,
            "a failed Back keeps its destination"
        );
        assert!(s.navigation.forward.is_empty());
    });
    std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.navigate_history(false, w, cx))
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).editor.as_ref().unwrap().read(cx).path == "api/a.txt"
    })
    .await;
    // Change focus before the detached source read can apply its result.
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.open_location(link(), Encoding::Utf16, true, false, w, cx);
            s.select_file(1, w, cx);
        })
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.editor.as_ref().unwrap().read(cx).path, "api/b.txt");
        assert_eq!(s.file_tabs[0].editor.read(cx).path, "api/a.txt");
        assert_eq!(s.file_tabs[1].editor.read(cx).path, "api/b.txt");
        assert!(s.navigation.back.is_empty());
        assert_eq!(s.navigation.forward.len(), 1);
    });
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.open_location(link(), Encoding::Utf16, true, false, w, cx);
            let mut latest = link();
            latest.target_uri = file_uri(&root.join("a.txt"));
            s.open_location(latest, Encoding::Utf16, true, false, w, cx);
        })
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).editor.as_ref().unwrap().read(cx).path == "api/a.txt"
    })
    .await;
    cx.update(|cx| {
        assert_eq!(shark.read(cx).file_tabs.len(), 2);
        assert!(
            shark
                .read(cx)
                .file_tabs
                .iter()
                .all(|t| t.editor.read(cx).path != "api/c.txt"),
            "only the newest pending navigation may replace a tab"
        );
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn language_edits_reuse_an_editor_opened_through_a_symlink(cx: &mut TestAppContext) {
    let _guard = ui_tests::UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (root, window, shark) = fixture(cx).await;
    std::os::unix::fs::symlink(root.join("a.txt"), root.join("alias.txt")).unwrap();
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.select_file(0, w, cx);
            s.open_path("api/alias.txt".into(), w, cx);
        })
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        let e = shark.read(cx).editor.as_ref().unwrap().read(cx);
        e.path == "api/alias.txt" && e.input.read(cx).value().as_ref() == "alpha\n"
    })
    .await;
    let alias = cx.update(|cx| shark.read(cx).editor.clone().unwrap());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.apply_workspace_edit(
                edits(&root, "fixed", "other"),
                Encoding::Utf16,
                root.clone(),
                Some(EditGuard::capture()),
                None,
                w,
                cx,
            );
        })
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        alias.read(cx).dirty
    })
    .await;
    cx.update(|cx| {
        assert_eq!(alias.read(cx).input.read(cx).value().as_ref(), "fixed\n");
        assert_eq!(
            shark.read(cx).file_tabs.len(),
            2,
            "the alias must not acquire a second buffer"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "alpha\n"
        );
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn jump_bar_folder_results_and_selection_stay_in_their_workspace(cx: &mut TestAppContext) {
    let _guard = ui_tests::UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, window, shark) = fixture(cx).await;
    let workspace = cx.update(|cx| shark.read(cx).active.clone());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.select_file(0, w, cx);
            s.folder_menu("api".into(), w, cx);
            s.active = Some("different-workspace".into());
        })
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        assert!(
            !w.has_active_dialog(cx),
            "a stale folder listing must not open a menu"
        );
        shark.update(cx, |s, cx| {
            s.active = workspace;
            s.folder_menu("api".into(), w, cx);
        });
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |w, cx| {
        w.has_active_dialog(cx)
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        // Keep a valid workspace while selecting the old menu: without the guard it opens b.txt.
        shark.update(cx, |s, _| {
            let mut other = s.active_ws().unwrap().clone();
            other.name = "different-workspace".into();
            s.state.workspaces.push(other);
            s.active = Some("different-workspace".into());
        });
        w.render_frame(cx);
        w.press("down", cx);
        w.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            shark.read(cx).editor.as_ref().unwrap().read(cx).path,
            "api/a.txt"
        )
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn retry_rediscovers_shared_editors_without_losing_unsaved_text(cx: &mut TestAppContext) {
    let _guard = ui_tests::UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (root, window, shark) = fixture(cx).await;
    std::fs::write(root.join("c.txt"), "charlie\n").unwrap();
    let project = crate::lsp::tools::Project {
        root: root.clone(),
        worktree: root.clone(),
        language_id: "python",
        backend: crate::lsp::tools::Backend::Pyrefly,
        toolchain: "original".into(),
        rust_toolchain: None,
        interpreter: None,
    };
    let session = |project: crate::lsp::tools::Project, cx: &mut Context<Shark>| {
        let client = crate::lsp::client::Client::start(
            std::process::Command::new("/usr/bin/true"),
            &root,
            serde_json::json!({}),
        )
        .unwrap();
        cx.new(|_| crate::lsp::Session::with_client(project, client))
    };
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            let shared = session(project.clone(), cx);
            let mut other_project = project.clone();
            other_project.toolchain = "unrelated".into();
            let unrelated = session(other_project.clone(), cx);
            s.register_session(shared.clone(), w, cx);
            s.register_session(unrelated.clone(), w, cx);
            let first = s.file_tabs[0].editor.clone();
            let second = s.file_tabs[1].editor.clone();
            let third = s.make_file_tab(
                "api/c.txt".into(),
                root.join("c.txt"),
                Some("charlie\n".into()),
                w,
                cx,
            );
            let third_editor = third.editor.clone();
            s.active_ws_mut()
                .unwrap()
                .open_tab(TabId::File("api/c.txt".into()));
            s.file_tabs.push(third);
            first.update(cx, |e, cx| {
                e.attach_language(shared.clone(), w, cx);
                e.replace_language_text("unsaved answer\n".into(), w, cx);
                e.input
                    .update(cx, |input, cx| input.set_selected_range(2..7, cx));
            });
            second.update(cx, |e, cx| e.attach_language(shared, w, cx));
            third_editor.update(cx, |e, cx| e.attach_language(unrelated.clone(), w, cx));
            s.editor = Some(first.clone());
            let tabs = s.active_ws().unwrap().tab_order.clone();
            s.restart_language(w, cx);
            assert!(first.read(cx).intelligence.is_none());
            assert!(second.read(cx).intelligence.is_none());
            assert_eq!(
                third_editor.read(cx).intelligence.as_ref().unwrap().session,
                unrelated
            );
            assert!(!s.language_sessions.contains_key(&project.session_key()));
            assert!(
                s.language_sessions
                    .contains_key(&other_project.session_key())
            );
            assert_eq!(
                first.read(cx).input.read(cx).value().as_ref(),
                "unsaved answer\n"
            );
            assert_eq!(first.read(cx).input.read(cx).selected_range(), 2..7);
            assert!(first.read(cx).dirty);
            assert_eq!(s.active_ws().unwrap().tab_order, tabs);
            assert_eq!(
                std::fs::read_to_string(root.join("a.txt")).unwrap(),
                "alpha\n"
            );
            // Test builds skip discovery/startup; the tools fixture covers the new identity.
            third_editor.update(cx, |e, _| e.intelligence = None);
        });
    })
    .unwrap();
}

/// A session over a peer that records Shark's messages and never answers: a reply would
/// wake GPUI's test scheduler from the client's thread, which it rejects as nondeterminism.
fn fake_session(root: &Path) -> (crate::lsp::Session, PathBuf) {
    let log = root.join("server.jsonrpc");
    let mut command = std::process::Command::new("/bin/sh");
    // fd 3 keeps stdout open, so the client sees a live, silent server.
    command
        .args(["-c", "exec 3>&1; exec cat >> \"$0\""])
        .arg(&log);
    let client = crate::lsp::client::Client::start(command, root, serde_json::json!({})).unwrap();
    let project = crate::lsp::tools::Project {
        root: root.to_owned(),
        worktree: root.to_owned(),
        language_id: "python",
        backend: crate::lsp::tools::Backend::Pyrefly,
        toolchain: String::new(),
        rust_toolchain: None,
        interpreter: None,
    };
    (crate::lsp::Session::with_client(project, client), log)
}

fn server_messages(log: &Path) -> Vec<serde_json::Value> {
    (std::fs::read_to_string(log)
        .unwrap_or_default()
        .split("Content-Length: "))
    .filter_map(|frame| serde_json::from_str(frame.split_once("\r\n\r\n")?.1).ok())
    .collect()
}

fn session_of(editor: &Entity<FileEditor>, cx: &App) -> Option<Entity<crate::lsp::Session>> {
    (editor.read(cx).intelligence.as_ref()).map(|l| l.session.clone())
}

async fn open_tab(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    shark: &Entity<Shark>,
    path: &'static str,
) -> Entity<FileEditor> {
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.open_path_tab(path.into(), w, cx))
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        (shark.read(cx).editor.as_ref())
            .is_some_and(|e| e.read(cx).path == path && e.read(cx).intelligence.is_some())
    })
    .await;
    cx.update(|cx| shark.read(cx).editor.clone().unwrap())
}

// One server serves a project's files; it survives the replacement of its only tab and a
// workspace switch, and shuts down after the parked idle window.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn language_sessions_are_shared_and_outlive_their_tabs(cx: &mut TestAppContext) {
    let _guard = ui_tests::UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (root, window, shark) = fixture(cx).await;
    for name in ["c.txt", "d.txt", "e.txt"] {
        std::fs::write(root.join(name), "echo\n").unwrap();
    }
    let (session, _) = fake_session(&root);
    let weak = cx
        .update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| {
                let session = cx.new(|_| session);
                s.register_session(session.clone(), w, cx);
                session.downgrade()
            })
        })
        .unwrap();
    // Only tabs may own the editors, or they would keep the server alive themselves.
    let c = open_tab(cx, window, &shark, "api/c.txt").await.downgrade();
    let d = open_tab(cx, window, &shark, "api/d.txt").await.downgrade();
    cx.update(|cx| {
        let session = |e: &WeakEntity<FileEditor>| session_of(&e.upgrade().unwrap(), cx);
        assert_eq!(session(&c), weak.upgrade());
        assert_eq!(session(&d), weak.upgrade(), "two files share one server");
    });
    // A server message reaches the user.
    let before = cx
        .update_window(window, |_, w, cx| w.notifications(cx).len())
        .unwrap();
    cx.update(|cx| {
        weak.upgrade().unwrap().update(cx, |_, cx| {
            cx.emit(crate::lsp::SessionEvent::Notification {
                method: "window/showMessage".into(),
                params: serde_json::json!({"type":1,"message":"rust-analyzer failed to load the workspace"}),
                companion: false,
            })
        })
    });
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        assert_eq!(w.notifications(cx).len(), before + 1)
    })
    .unwrap();
    // Closing one of its files keeps the server, and stops one no file uses; replacing the
    // only other file keeps it too.
    std::fs::create_dir_all(root.join("node_modules")).unwrap();
    let (unused, _) = fake_session(&root.join("node_modules"));
    let unused = cx
        .update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| {
                let unused = cx.new(|_| unused);
                s.register_session(unused.clone(), w, cx);
                unused.downgrade()
            })
        })
        .unwrap();
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.close_editor(d.upgrade().unwrap(), w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    assert!(weak.upgrade().is_some());
    assert!(unused.upgrade().is_none(), "no open file used it");
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            let range = Range::new(Position::new(0, 0), Position::new(0, 1));
            let link = lsp_types::LocationLink {
                origin_selection_range: None,
                target_uri: file_uri(&root.join("e.txt")),
                target_range: range,
                target_selection_range: range,
            };
            s.open_location(link, Encoding::Utf16, true, false, w, cx)
        })
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        (shark.read(cx).editor.as_ref())
            .is_some_and(|e| e.read(cx).path == "api/e.txt" && e.read(cx).intelligence.is_some())
    })
    .await;
    cx.update(|cx| {
        let s = shark.read(cx);
        assert!(c.upgrade().is_none(), "c.txt was replaced");
        assert_eq!(session_of(s.editor.as_ref().unwrap(), cx), weak.upgrade());
    });
    // Switching workspaces parks the server, and coming back resumes it.
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.drop_editor(cx);
            assert!(s.language_sessions.is_empty());
            s.unpark_sessions(w, cx);
            assert!(
                s.language_sessions
                    .values()
                    .any(|(e, _)| Some(e) == weak.upgrade().as_ref())
            );
            s.drop_editor(cx);
        })
    })
    .unwrap();
    cx.run_until_parked();
    assert!(weak.upgrade().is_some(), "a parked server stays up");
    cx.executor()
        .advance_clock(super::editor_pane::PARKED_SESSIONS + Duration::from_secs(1));
    cx.run_until_parked();
    assert!(weak.upgrade().is_none(), "an idle parked server shuts down");
}

// F2 renames to the typed name; F12 asks for the definition; Option-Shift-F formats with the
// buffer's own indentation.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn language_keys_send_rename_definition_and_buffer_formatting(cx: &mut TestAppContext) {
    let _guard = ui_tests::UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (root, window, shark) = fixture(cx).await;
    let text = "answer {\n  one\n    two\n}\n";
    std::fs::write(root.join("c.txt"), text).unwrap();
    let (session, log) = fake_session(&root);
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.set_vim(false, cx);
            let session = cx.new(|_| session);
            s.register_session(session, w, cx);
        })
    })
    .unwrap();
    let editor = open_tab(cx, window, &shark, "api/c.txt").await;
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        let language = editor.read(cx).intelligence.as_ref().unwrap();
        language.snapshot.lock().unwrap().text.as_ref() == text
    })
    .await;
    let focus = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, w, cx| {
            editor.update(cx, |e, cx| {
                e.input.update(cx, |i, cx| {
                    i.set_selected_range(2..2, cx);
                    i.focus(w, cx);
                })
            });
            w.render_frame(cx);
        })
        .unwrap();
    };
    let sent = |method: &'static str| {
        let log = log.clone();
        move |_: &mut Window, _: &mut App| {
            server_messages(&log)
                .into_iter()
                .any(|m| m["method"] == method)
        }
    };
    focus(cx);
    cx.update_window(window, |_, w, cx| w.press("f2", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.has_active_dialog(cx));
        w.input("result", cx);
        w.press("enter", cx);
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), sent("textDocument/rename"))
        .await;
    let rename = server_messages(&log)
        .into_iter()
        .find(|m| m["method"] == "textDocument/rename")
        .unwrap();
    assert_eq!(
        rename["params"]["newName"], "result",
        "the dialog starts focused, with the symbol selected for replacement"
    );
    focus(cx);
    cx.update_window(window, |_, w, cx| w.press("f12", cx))
        .unwrap();
    cx.wait_for(
        window,
        Duration::from_secs(10),
        sent("textDocument/definition"),
    )
    .await;
    focus(cx);
    cx.update_window(window, |_, w, cx| w.press("alt-shift-f", cx))
        .unwrap();
    cx.wait_for(
        window,
        Duration::from_secs(10),
        sent("textDocument/formatting"),
    )
    .await;
    let format = server_messages(&log)
        .into_iter()
        .find(|m| m["method"] == "textDocument/formatting")
        .unwrap();
    assert_eq!(
        format["params"]["options"],
        serde_json::json!({"tabSize":2,"insertSpaces":true})
    );
    // Vim's `g d` is the same request.
    cx.update(|cx| shark.update(cx, |s, cx| s.set_vim(true, cx)));
    focus(cx);
    cx.update_window(window, |_, w, cx| {
        w.press("g", cx);
        w.press("d", cx);
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, _| {
        (server_messages(&log).iter())
            .filter(|m| m["method"] == "textDocument/definition")
            .count()
            == 2
    })
    .await;
}

// Closing a tab keeps a server whose project still has a file that has not attached yet (a
// restored tab still discovering), and stops it once its last file closes.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn closing_a_tab_keeps_servers_of_files_still_discovering(cx: &mut TestAppContext) {
    let _guard = ui_tests::UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (root, window, shark) = fixture(cx).await;
    std::fs::create_dir_all(root.join("pending")).unwrap();
    std::fs::write(root.join("pending/p.txt"), "papa\n").unwrap();
    let (session, _) = fake_session(&root.join("pending"));
    let weak = cx
        .update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| {
                let session = cx.new(|_| session);
                s.register_session(session.clone(), w, cx);
                session.downgrade()
            })
        })
        .unwrap();
    let p = open_tab(cx, window, &shark, "api/pending/p.txt").await;
    cx.update(|cx| p.update(cx, |e, _| e.intelligence = None));
    let close = |cx: &mut TestAppContext, editor: Entity<FileEditor>| {
        cx.update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| s.close_editor(editor, w, cx))
        })
        .unwrap();
        cx.run_until_parked();
    };
    let a = cx.update(|cx| shark.read(cx).file_tabs[0].editor.clone());
    close(cx, a);
    assert!(weak.upgrade().is_some(), "p.txt may still attach to it");
    close(cx, p);
    assert!(weak.upgrade().is_none());
}

// A saved tab or a definition pointing at a FIFO is refused instead of blocking the read
// (restore reads every saved tab in one task, so one FIFO would hold them all back).
#[test]
fn read_source_refuses_a_fifo_without_blocking() {
    let fifo = std::env::temp_dir().join(format!("shark-fifo-{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&fifo);
    let made = std::process::Command::new("mkfifo").arg(&fifo).status();
    assert!(made.unwrap().success());
    let (tx, rx) = std::sync::mpsc::channel();
    let path = fifo.clone();
    std::thread::spawn(move || {
        let _ = tx.send(super::intelligence::read_source(&path).map_err(|e| e.to_string()));
    });
    let result = rx.recv_timeout(Duration::from_secs(5));
    let _ = std::fs::remove_file(&fifo);
    let result = result.expect("read_source blocked on a FIFO");
    assert_eq!(result.unwrap_err(), "Not a regular source file");
}

#[test]
fn formatting_options_follow_the_buffer_indentation() {
    let options = |text: &str| super::intelligence::formatting_options(text);
    let spaces = |n: usize| serde_json::json!({"tabSize":n,"insertSpaces":true});
    assert_eq!(options(""), spaces(4));
    assert_eq!(options("a {\n  b {\n    c\n  }\n}\n"), spaces(2));
    // JSDoc's one-space continuation lines do not set the step.
    assert_eq!(
        options("/**\n * doc\n */\nf {\n  a\n  b {\n    c\n  }\n}\n"),
        spaces(2)
    );
    assert_eq!(
        options("/**\n * doc\n */\nfunction f() {\n  return 1;\n}\n"),
        spaces(2)
    );
    assert_eq!(options("def f():\n    return 1\n"), spaces(4));
    assert_eq!(
        options("func f() {\n\tx()\n}\n"),
        serde_json::json!({"tabSize":4,"insertSpaces":false})
    );
    // A generator method starts with `*` but is code, not a comment body.
    assert_eq!(
        options("class C {\n\t*values() {}\n}\n"),
        serde_json::json!({"tabSize":4,"insertSpaces":false})
    );
    assert_eq!(options("class C {\n  *values() {}\n}\n"), spaces(2));
}

// Back skips a file deleted since; quick repeated presses walk one entry each.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn history_skips_deleted_files_and_counts_quick_presses(cx: &mut TestAppContext) {
    let _guard = ui_tests::UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (root, window, shark) = fixture(cx).await;
    std::fs::write(root.join("c.txt"), "charlie\n").unwrap();
    std::fs::write(root.join("d.txt"), "delta\n").unwrap();
    let go = |cx: &mut TestAppContext, name: &str| {
        let range = Range::new(Position::new(0, 0), Position::new(0, 1));
        let link = lsp_types::LocationLink {
            origin_selection_range: None,
            target_uri: file_uri(&root.join(name)),
            target_range: range,
            target_selection_range: range,
        };
        cx.update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| {
                s.open_location(link, Encoding::Utf16, true, false, w, cx)
            })
        })
        .unwrap();
    };
    let at = async |cx: &mut TestAppContext, path: &'static str| {
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            (shark.read(cx).editor.as_ref()).is_some_and(|e| e.read(cx).path == path)
        })
        .await;
    };
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.select_file(0, w, cx))
    })
    .unwrap();
    // a.txt → c.txt → d.txt, each in place of the last.
    go(cx, "c.txt");
    at(cx, "api/c.txt").await;
    go(cx, "d.txt");
    at(cx, "api/d.txt").await;
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.navigate_history(false, w, cx);
            s.navigate_history(false, w, cx);
        })
    })
    .unwrap();
    at(cx, "api/a.txt").await;
    cx.update(|cx| {
        let s = shark.read(cx);
        assert!(s.navigation.back.is_empty());
        let forward: Vec<_> = (s.navigation.forward.iter())
            .map(|v| v.path.file_name().unwrap().to_owned())
            .collect();
        assert_eq!(
            forward,
            ["d.txt", "c.txt"],
            "two presses went back two files"
        );
    });
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.navigate_history(true, w, cx))
    })
    .unwrap();
    at(cx, "api/c.txt").await;
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.navigate_history(true, w, cx))
    })
    .unwrap();
    at(cx, "api/d.txt").await;
    std::fs::remove_file(root.join("c.txt")).unwrap();
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.navigate_history(false, w, cx))
    })
    .unwrap();
    at(cx, "api/a.txt").await;
    cx.update(|cx| {
        let s = shark.read(cx);
        assert!(
            s.navigation.back.is_empty(),
            "the deleted c.txt left history"
        );
        assert_eq!(s.navigation.forward.len(), 1);
    });
}

// Dependency sources, including installed packages inside the worktree, open read-only with
// the server that found them, and Back restores the file's tab slot.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn dependency_sources_are_read_only_share_the_session_and_keep_tab_slots(
    cx: &mut TestAppContext,
) {
    let _guard = ui_tests::UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (root, window, shark) = fixture(cx).await;
    let dependency = root.join("node_modules/dep/index.txt");
    std::fs::create_dir_all(dependency.parent().unwrap()).unwrap();
    std::fs::write(&dependency, "export\n").unwrap();
    let (session, _) = fake_session(&root);
    let session = cx
        .update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| {
                s.set_vim(false, cx);
                let session = cx.new(|_| session);
                s.register_session(session.clone(), w, cx);
                // a.txt is the first file tab, before the diff and b.txt.
                let a = s.file_tabs[0].editor.clone();
                a.update(cx, |e, cx| e.attach_language(session.clone(), w, cx));
                s.select_file(0, w, cx);
                session
            })
        })
        .unwrap();
    let tabs = cx.update(|cx| shark.read(cx).active_ws().unwrap().tab_order.clone());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            let range = Range::new(Position::new(0, 0), Position::new(0, 6));
            let link = lsp_types::LocationLink {
                origin_selection_range: None,
                target_uri: file_uri(&dependency),
                target_range: range,
                target_selection_range: range,
            };
            s.open_location(link, Encoding::Utf16, true, false, w, cx)
        })
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        (shark.read(cx).editor.as_ref()).is_some_and(|e| e.read(cx).external)
    })
    .await;
    cx.update(|cx| {
        let e = shark.read(cx).editor.clone().unwrap();
        assert_eq!(session_of(&e, cx), Some(session.clone()));
    });
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.navigate_history(false, w, cx))
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        (shark.read(cx).editor.as_ref()).is_some_and(|e| e.read(cx).path == "api/a.txt")
    })
    .await;
    cx.update(|cx| {
        assert_eq!(shark.read(cx).active_ws().unwrap().tab_order, tabs);
    });
}
