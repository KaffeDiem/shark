use super::*;
use crate::app::intelligence_tests::{editors, fixture};

// A folder in the jump bar lists its entries, folders first, starting at the open file;
// picking a file opens it in place of the current one.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn jump_bar_folder_lists_its_files(cx: &mut TestAppContext) {
    let _guard = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

// The editor uses 14px and the diff 13px on 21px rows; both follow the saved zoom.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn editor_text_size_defaults_and_zoom_steps(cx: &mut TestAppContext) {
    let _guard = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, window, shark) = fixture(cx).await;
    let editors = cx.update(|cx| editors(&shark, cx));
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.select_file(0, w, cx));
        editors[0].update(cx, |e, cx| e.input.update(cx, |i, cx| i.focus(w, cx)));
        w.render_frame(cx);
    })
    .unwrap();
    let row =
        |cx: &mut TestAppContext| cx.update(|cx| editors[0].read(cx).input.read(cx).line_height());
    assert_eq!(row(cx), Some(px(21.)));
    cx.update(|cx| {
        assert_eq!(crate::editor::code_font(cx), (px(14.), px(21.)));
        assert_eq!(crate::editor::diff_font(cx), (px(13.), px(21.)));
    });
    cx.update_window(window, |_, w, cx| {
        w.press("cmd-=", cx);
        w.press("cmd-=", cx);
    })
    .unwrap();
    cx.update(|cx| {
        assert_eq!(crate::editor::code_font(cx), (px(16.), px(24.)));
        let (size, row) = crate::editor::diff_font(cx);
        assert!((f32::from(size) - 14.857143).abs() < 0.0001);
        assert_eq!(row, px(24.));
        assert_eq!(shark.read(cx).state.editor_font_size, Some(16.));
    });
    assert_eq!(row(cx), Some(px(24.)));
    cx.update_window(window, |_, w, cx| {
        for _ in 0..20 {
            w.press("cmd--", cx);
        }
    })
    .unwrap();
    cx.update(|cx| {
        assert_eq!(crate::editor::code_font(cx), (px(10.), px(15.)));
        let (size, row) = crate::editor::diff_font(cx);
        assert!((f32::from(size) - 9.285714).abs() < 0.0001);
        assert_eq!(row, px(15.));
    });
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.step_editor_font(0., w, cx))
    })
    .unwrap();
    cx.update(|cx| {
        assert_eq!(crate::editor::code_font(cx), (px(14.), px(21.)));
        assert_eq!(shark.read(cx).state.editor_font_size, None);
        assert_eq!(crate::editor::diff_font(cx), (px(13.), px(21.)));
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn jump_bar_folder_results_and_selection_stay_in_their_workspace(cx: &mut TestAppContext) {
    let _guard = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        // Another existing workspace is active when a row of the menu built for the first one is
        // picked: the pick belongs to the old workspace and must be dropped, not opened here.
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
