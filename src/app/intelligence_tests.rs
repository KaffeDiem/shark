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
            s.open_location(link(&external, 50, 3, 13), Encoding::Utf16, true, w, cx)
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
