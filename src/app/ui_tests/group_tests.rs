//! Side-by-side tab groups: pointer drags, keyboard routing, quick open and restoration.
use super::*;
use gpui_kit::test::TestEventExt;

const A: &str = "api/a.txt";
const B: &str = "api/b.txt";

fn file(p: &str) -> TabId {
    TabId::File(p.into())
}

/// One workspace "groups" with a chat, files a and b, Diff and Git; and an empty "other".
fn fixture(home: &Path) -> State {
    let repos = repos(home, &["api"]);
    let mut state = State {
        repos: repos.clone(),
        ..Default::default()
    };
    for name in ["groups", "other"] {
        let mut ws = Workspace {
            name: name.into(),
            branch: "main".into(),
            repos: repos
                .iter()
                .map(|r| crate::state::WsRepo {
                    name: r.name.clone(),
                    src: r.path.clone(),
                    base: "main".into(),
                    branch: None,
                })
                .collect(),
            ..Default::default()
        };
        ws.migrate();
        init_repo(&ws.dir().join("api"));
        for f in ["a.txt", "b.txt", "c.txt", "d.txt"] {
            fs::write(ws.dir().join("api").join(f), format!("{f}\n")).unwrap();
        }
        if name == "groups" {
            ws.open_tab(file(A));
            ws.diff_closed = false;
            ws.open_tab(TabId::Diff);
            ws.open_tab(file(B));
            ws.git_closed = false;
            ws.open_tab(TabId::Git);
        }
        state.workspaces.push(ws);
    }
    state.last_open = Some(("groups".into(), "chat".into()));
    state
}

fn options(width: f32) -> WindowOptions {
    WindowOptions {
        app_owns_titlebar_drag: true,
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(0.), px(0.)),
            size(px(width), px(820.)),
        ))),
        ..Default::default()
    }
}

fn element(id: &TabId) -> SharedString {
    match id {
        TabId::Chat(id) => format!("chat-tab-{id}"),
        TabId::Diff => "diff-tab".into(),
        TabId::Git => "git-tab".into(),
        TabId::File(path) => format!("file-tab-{path}"),
    }
    .into()
}

/// Presses on `from`, drags past the threshold, then onto what `to` finds once the drag
/// shows its targets; releases there unless `release` is false.
fn drag_tab(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    from: &TabId,
    to: impl Fn(&Window) -> Point<Pixels>,
    release: bool,
) {
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let start = w.find(element(from)).bounds().center();
        w.pointer_down(start, MouseButton::Left, cx);
        w.pointer_move(start + point(px(8.), px(0.)), Some(MouseButton::Left), cx);
        w.render_frame(cx);
        let end = to(w);
        w.pointer_move(end, Some(MouseButton::Left), cx);
        w.render_frame(cx);
        w.pointer_move(end + point(px(1.), px(0.)), Some(MouseButton::Left), cx);
        if release {
            w.pointer_up(end, MouseButton::Left, cx);
        }
    })
    .unwrap();
    cx.run_until_parked();
}

fn center(id: &'static str) -> impl Fn(&Window) -> Point<Pixels> {
    move |w| w.find(id).bounds().center()
}

fn groups(shark: &Entity<Shark>, cx: &mut TestAppContext) -> [Vec<TabId>; 2] {
    cx.update(|cx| {
        let ws = shark.read(cx).active_ws().unwrap().clone();
        [ws.group_tabs(0), ws.group_tabs(1)]
    })
}

/// The dotted target appears only while a tab drags; dropping on it splits with the tab on
/// the right, focused, its buffer and the chat's draft untouched. Escape, outside releases and
/// stale identities change nothing; strips take drops on tabs and in their trailing space,
/// and the last tab moved out collapses its group.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn drag_splits_moves_and_collapses(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(cx, fixture(&home), options(1440.));
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).file_tabs.len() == 2
    })
    .await;
    let chat = cx.update(|cx| shark.read(cx).active_chat().unwrap().clone());
    on(cx, &|s, w, cx| s.select_chat(0, w, cx));
    cx.update_window(window, |_, w, cx| w.input("draft stays", cx))
        .unwrap();
    on(cx, &|s, w, cx| s.select_file(0, w, cx));
    let editor = cx.update(|cx| shark.read(cx).editor.clone().unwrap());
    cx.update_window(window, |_, w, cx| w.input("unsaved ", cx))
        .unwrap();
    let text = cx.update(|cx| editor.read(cx).input.read(cx).value().to_string());
    let before = groups(&shark, cx);
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("split-target").is_none(), "only while dragging");
    })
    .unwrap();

    // Escape abandons the drag even when released on the target.
    drag_tab(cx, window, &TabId::Diff, center("split-target"), false);
    cx.update_window(window, |_, w, _| {
        assert!(
            w.try_find("split-target").is_some(),
            "dotted target during a drag"
        );
    })
    .unwrap();
    press(cx, &["escape"]);
    cx.update_window(window, |_, w, cx| {
        let end = w.find("titlebar-empty").bounds().center();
        w.pointer_up(end, MouseButton::Left, cx);
        w.render_frame(cx);
        assert!(w.try_find("split-target").is_none());
    })
    .unwrap();
    assert_eq!(groups(&shark, cx), before);
    // Released outside: cancelled.
    drag_tab(
        cx,
        window,
        &TabId::Diff,
        |_| point(px(700.), px(600.)),
        true,
    );
    assert_eq!(groups(&shark, cx), before);
    assert!(!State::load().workspaces[0].split());
    // With one group, its trailing space takes the tab to the end.
    drag_tab(
        cx,
        window,
        &TabId::Diff,
        |w| {
            let b = w.find("titlebar-empty").bounds();
            point(b.right() - px(8.), b.center().y)
        },
        true,
    );
    let before = groups(&shark, cx);
    assert_eq!(before[0].last(), Some(&TabId::Diff));
    assert!(before[1].is_empty());

    // Onto the target: a split, the file on the right with its buffer.
    drag_tab(cx, window, &file(A), center("split-target"), true);
    cx.update_window(window, |_, w, cx| {
        let s = shark.read(cx);
        let ws = s.active_ws().unwrap();
        assert!(ws.split());
        assert_eq!(ws.group_tabs(1), [file(A)]);
        assert_eq!(ws.layout.focused, 1);
        assert_eq!(ws.layout.ratio, None, "half and half");
        assert_eq!(s.editor.as_ref(), Some(&editor));
        assert_eq!(s.tab, Tab::File);
        assert!(editor.read(cx).is_focused(w, cx));
        assert!(editor.read(cx).dirty);
        assert_eq!(editor.read(cx).input.read(cx).value().as_ref(), text);
        assert_eq!(
            chat.read(cx).composer.read(cx).value().as_ref(),
            "draft stays"
        );
        assert!(State::load().workspaces[0].split());
    })
    .unwrap();
    // Strips line up with their groups; no second split target.
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.render_frame(cx);
        let [left, right] = shark.read(cx).group_bounds.get();
        let strip = w.find("chat-tabs-1").bounds();
        assert!(
            (strip.left() - right.left()).abs() < px(2.),
            "{strip:?} over {right:?}"
        );
        assert!(w.find("chat-tabs").bounds().right() <= left.right() + px(1.));
        assert!(
            (left.size.width - right.size.width).abs() < px(2.),
            "equal widths"
        );
    })
    .unwrap();
    drag_tab(cx, window, &TabId::Diff, center("titlebar-empty"), false);
    cx.update_window(window, |_, w, cx| {
        assert!(w.try_find("split-target").is_none(), "already split");
        let end = w.find("titlebar-empty").bounds().center();
        w.pointer_up(end, MouseButton::Left, cx);
    })
    .unwrap();
    cx.run_until_parked();
    // Released in the right strip's trailing space: Diff goes last there.
    assert_eq!(groups(&shark, cx)[1], [file(A), TabId::Diff]);
    // Onto a tab of the other strip: before it.
    drag_tab(cx, window, &TabId::Git, center("diff-tab"), true);
    assert_eq!(groups(&shark, cx)[1], [file(A), TabId::Git, TabId::Diff]);
    cx.update(|cx| assert_eq!(shark.read(cx).tab, Tab::Feature(Feature::Git)));
    // Within a strip: into a slot.
    drag_tab(cx, window, &TabId::Diff, center("file-tab-api/a.txt"), true);
    assert_eq!(groups(&shark, cx)[1], [TabId::Diff, file(A), TabId::Git]);
    // Stale identities change nothing.
    let now = groups(&shark, cx);
    on(cx, &|s, w, cx| {
        s.move_to_group("other", &TabId::Diff, 0, None, w, cx);
        s.move_to_group("groups", &file("api/gone"), 0, None, w, cx);
        s.move_to_group("groups", &TabId::Diff, 0, Some(&file(A)), w, cx);
    });
    assert_eq!(groups(&shark, cx), now);
    // Into the left strip's trailing space, one by one: the right group collapses.
    for t in [TabId::Diff, file(A), TabId::Git] {
        drag_tab(cx, window, &t, center("titlebar-gap"), true);
    }
    cx.update(|cx| {
        let s = shark.read(cx);
        let ws = s.active_ws().unwrap();
        assert!(!ws.split());
        assert_eq!(ws.layout.focused, 0);
        assert_eq!(ws.group_tabs(0).last(), Some(&TabId::Git));
        assert!(editor.read(cx).dirty, "still the same buffer");
        assert_eq!(s.file_tabs.len(), 2);
    });
}

/// cmd-\ splits and moves back; ctrl-tab, ctrl-1…9, chat cycling, cmd-t, cmd-w and cmd-s act
/// on the focused group; focus traversal walks both groups; Git and Diff open in the focused
/// group or focus the group they are in; running chats survive moves.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn keyboard_routes_to_focused_group(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let mut state = fixture(&home);
    state.workspaces[0].git_closed = true;
    state.workspaces[0].normalize_tabs();
    let (window, shark) = boot(cx, state, options(1440.));
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).file_tabs.len() == 2
    })
    .await;
    let chat = cx.update(|cx| shark.read(cx).active_chat().unwrap().clone());
    cx.update(|cx| chat.update(cx, |c, _| c.busy = true));
    // The lone tab of a fresh workspace can't split.
    on(cx, &|s, w, cx| s.open_workspace_named("other", w, cx));
    press(cx, &["cmd-\\"]);
    cx.update(|cx| assert!(!shark.read(cx).active_ws().unwrap().split()));
    on(cx, &|s, w, cx| s.open_workspace_named("groups", w, cx));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).file_tabs.len() == 2
    })
    .await;

    // Chat to a new right group.
    on(cx, &|s, w, cx| s.select_chat(0, w, cx));
    press(cx, &["cmd-\\"]);
    cx.update(|cx| {
        let s = shark.read(cx);
        let ws = s.active_ws().unwrap();
        assert_eq!(ws.group_tabs(1), [TabId::Chat("groups".into())]);
        assert_eq!(ws.layout.focused, 1);
        assert_eq!(s.active_chat(), Some(&chat));
        assert!(chat.read(cx).working(), "still running");
    });
    // ctrl-1 and ctrl-tab stay in the right group; chat cycling stays on its chat.
    press(cx, &["ctrl-1", "ctrl-tab", "cmd-shift-]"]);
    cx.update(|cx| assert_eq!(shark.read(cx).active_chat(), Some(&chat)));
    // cmd-t adds a chat to the right group, cmd-w closes it there.
    press(cx, &["cmd-t"]);
    let new = cx.update(|cx| {
        let ws = shark.read(cx).active_ws().unwrap().clone();
        assert_eq!(ws.group_tabs(1).len(), 2);
        ws.group_tabs(1)[1].clone()
    });
    press(cx, &["cmd-w"]);
    cx.update(|cx| {
        let ws = shark.read(cx).active_ws().unwrap().clone();
        assert!(!ws.tab_order.contains(&new));
        assert_eq!(ws.layout.selected[1], Some(TabId::Chat("groups".into())));
    });
    // The left group has no chats: chat cycling there does nothing.
    on(cx, &|s, w, cx| s.select_file(0, w, cx));
    cx.update(|cx| assert_eq!(shark.read(cx).focused_group(), 0));
    press(cx, &["cmd-shift-]"]);
    cx.update(|cx| {
        assert_eq!(shark.read(cx).tab, Tab::File);
        assert_eq!(shark.read(cx).focused_group(), 0);
    });
    press(cx, &["ctrl-2"]);
    cx.update(|cx| assert_eq!(shark.read(cx).tab, Tab::Feature(Feature::Diff)));
    press(cx, &["ctrl-tab"]);
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.tab, Tab::File);
        assert_eq!(s.editor.as_ref().unwrap().read(cx).path, B);
    });
    // Git, closed, opens in the focused (left) group; once open, cmd-shift-g from the right
    // focuses the left group again.
    press(cx, &["cmd-shift-g"]);
    cx.update(|cx| {
        let ws = shark.read(cx).active_ws().unwrap().clone();
        assert_eq!(ws.group_of(&TabId::Git), 0);
        assert_eq!(ws.group_tabs(0).last(), Some(&TabId::Git));
    });
    on(cx, &|s, w, cx| s.select_chat(0, w, cx));
    cx.update(|cx| assert_eq!(shark.read(cx).focused_group(), 1));
    press(cx, &["cmd-shift-g"]);
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.focused_group(), 0);
        assert_eq!(s.tab, Tab::Feature(Feature::Git));
        assert_eq!(s.active_ws().unwrap().group_of(&TabId::Git), 0);
    });
    // Focus traversal: sidebar, the left group's panes, the right group's chat, the rail.
    on(cx, &|s, w, cx| s.select_file(0, w, cx));
    on(cx, &|s, w, cx| s.focus_pane(Pane::Sidebar, w, cx));
    press(cx, &["cmd-alt-right"]);
    cx.update_window(window, |_, w, cx| {
        assert_eq!(shark.read(cx).focused_pane(w, cx), Some(Pane::Editor(0)));
    })
    .unwrap();
    press(cx, &["cmd-alt-right"]);
    cx.update_window(window, |_, w, cx| {
        let s = shark.read(cx);
        assert_eq!(s.focused_pane(w, cx), Some(Pane::Chat(1)));
        assert_eq!(s.focused_group(), 1, "focus moved the group");
        assert_eq!(s.tab, Tab::Chat);
    })
    .unwrap();
    press(cx, &["cmd-alt-left"]);
    cx.update_window(window, |_, w, cx| {
        let s = shark.read(cx);
        assert_eq!(s.focused_pane(w, cx), Some(Pane::Editor(0)));
        assert_eq!(s.focused_group(), 0);
    })
    .unwrap();
    // cmd-s saves the focused group's file.
    let editor = cx.update(|cx| shark.read(cx).editor.clone().unwrap());
    cx.update_window(window, |_, w, cx| w.input("x", cx))
        .unwrap();
    press(cx, &["cmd-s"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        !editor.read(cx).dirty
    })
    .await;
    // Moving the right group's only tab back collapses it; the chat keeps running.
    on(cx, &|s, w, cx| s.select_chat(0, w, cx));
    press(cx, &["cmd-\\"]);
    cx.update(|cx| {
        let s = shark.read(cx);
        assert!(!s.active_ws().unwrap().split());
        assert_eq!(s.active_chat(), Some(&chat));
        assert!(chat.read(cx).working());
    });
    cx.update(|cx| chat.update(cx, |c, _| c.busy = false));
}

/// Quick open replaces the target group's file, appends with cmd-enter or when the group
/// shows something else, selects an open path, keeps a cancelled discard, and gives up when
/// the workspace changed. The footer hints are there.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn quick_open_targets_its_group(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(cx, fixture(&home), options(1440.));
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).file_tabs.len() == 2
    })
    .await;
    // b to the right; a stays left.
    on(cx, &|s, w, cx| {
        s.move_to_group("groups", &file(B), 1, None, w, cx)
    });
    let order = |cx: &mut TestAppContext| groups(&shark, cx);
    // Enter replaces the right group's file, in its slot.
    on(cx, &|s, w, cx| {
        s.open_in_group("api/c.txt".into(), "groups", 1, false, w, cx)
    });
    assert_eq!(order(cx)[1], [file("api/c.txt")]);
    // cmd-enter appends.
    on(cx, &|s, w, cx| {
        s.open_in_group(B.into(), "groups", 1, true, w, cx)
    });
    assert_eq!(order(cx)[1], [file("api/c.txt"), file(B)]);
    // An open path selects its tab, wherever it is.
    on(cx, &|s, w, cx| {
        s.open_in_group(A.into(), "groups", 1, false, w, cx)
    });
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.focused_group(), 0);
        assert_eq!(s.editor.as_ref().unwrap().read(cx).path, A);
    });
    assert_eq!(order(cx)[1], [file("api/c.txt"), file(B)]);
    // A group showing Diff gets a new tab.
    on(cx, &|s, w, cx| s.show(Tab::Feature(Feature::Diff), w, cx));
    on(cx, &|s, w, cx| {
        s.open_in_group(B.into(), "groups", 0, false, w, cx)
    });
    cx.update(|cx| assert_eq!(shark.read(cx).focused_group(), 1, "b was open on the right"));
    // Dirty replacement asks; cancel keeps the buffer and its tab.
    on(cx, &|s, w, cx| s.select_id(file(B), w, cx));
    let dirty = cx.update(|cx| shark.read(cx).editor.clone().unwrap());
    cx.update_window(window, |_, w, cx| w.input("keep ", cx))
        .unwrap();
    let before = order(cx);
    on(cx, &|s, w, cx| {
        let g = s.focused_group();
        s.open_in_group("api/d.txt".into(), "groups", g, false, w, cx)
    });
    cx.update_window(window, |_, w, cx| assert!(w.has_active_dialog(cx)))
        .unwrap();
    press(cx, &["escape"]);
    assert_eq!(order(cx), before);
    cx.update(|cx| {
        assert!(dirty.read(cx).dirty);
        assert_eq!(shark.read(cx).editor.as_ref(), Some(&dirty));
    });
    // Captured for another workspace: nothing opens.
    on(cx, &|s, w, cx| {
        s.open_in_group("api/c.txt".into(), "other", 0, false, w, cx)
    });
    assert_eq!(order(cx), before);

    // The picker itself: footer hints, then cmd-enter from the right group appends there.
    on(cx, &|s, w, cx| s.select_file(0, w, cx));
    let g = cx.update(|cx| shark.read(cx).focused_group());
    press(cx, &["cmd-p"]);
    cx.wait_for(window, Duration::from_secs(10), |w, cx| {
        w.try_find("new-tab-hint").is_some()
            && !w.find_all("file-picker-hints").is_empty()
            && shark.read(cx).active.is_some()
    })
    .await;
    cx.update_window(window, |_, w, cx| w.input("a.txt", cx))
        .unwrap();
    cx.run_until_parked();
    let tabs = order(cx)[g].len();
    press(cx, &["cmd-enter"]);
    cx.update_window(window, |_, w, cx| assert!(!w.has_active_dialog(cx)))
        .unwrap();
    // a.txt is already open: its tab is selected, nothing is added.
    assert_eq!(order(cx)[g].len(), tabs);
    cx.update(|cx| assert_eq!(shark.read(cx).editor.as_ref().unwrap().read(cx).path, A));
    // Enter in the picker replaces the selected a.txt in its slot.
    let at = order(cx)[g].iter().position(|t| *t == file(A)).unwrap();
    press(cx, &["cmd-p"]);
    cx.wait_for(window, Duration::from_secs(10), |w, _| {
        w.try_find("new-tab-hint").is_some()
    })
    .await;
    cx.update_window(window, |_, w, cx| w.input("d.txt", cx))
        .unwrap();
    cx.run_until_parked();
    press(cx, &["enter"]);
    assert_eq!(order(cx)[g][at], file("api/d.txt"));
    assert!(!order(cx)[g].contains(&file(A)));
}

/// A split, both selections (a file included) and the divider survive a workspace switch and
/// a restart; a selection made while files restore isn't overwritten.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn layout_restores_after_switch_and_restart(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let mut state = fixture(&home);
    let ws = &mut state.workspaces[0];
    assert!(ws.move_to_group(&file(B), 1, None));
    assert!(ws.move_to_group(&TabId::Diff, 1, None));
    ws.select_tab(&file(B));
    ws.select_tab(&file(A));
    ws.layout.ratio = Some(0.3);
    let saved = ws.layout.clone();
    let (window, shark) = boot(cx, state, options(1440.));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        let s = shark.read(cx);
        s.file_tabs.len() == 2 && s.editor.is_some()
    })
    .await;
    let check = |shark: &Entity<Shark>, cx: &mut TestAppContext| {
        cx.update(|cx| {
            let s = shark.read(cx);
            let ws = s.active_ws().unwrap();
            assert_eq!(ws.layout.selected, saved.selected);
            assert_eq!(ws.layout.focused, 0);
            assert_eq!(s.tab, Tab::File);
            assert_eq!(s.editor.as_ref().unwrap().read(cx).path, A);
            assert_eq!(s.group_selection(1, cx), Some(file(B)));
        })
    };
    check(&shark, cx);
    // The divider takes the saved ratio once laid out.
    cx.update_window(window, |_, w, cx| {
        for _ in 0..3 {
            w.render_frame(cx);
        }
        let [l, r] = shark.read(cx).group_bounds.get();
        let share = l.size.width / (l.size.width + r.size.width);
        assert!((share - 0.3).abs() < 0.02, "{share}");
    })
    .unwrap();
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| s.open_workspace_named("other", w, cx));
    cx.update(|cx| assert!(!shark.read(cx).split_shown()));
    // Back, and a newer selection made before the files land wins.
    on(cx, &|s, w, cx| {
        s.open_workspace_named("groups", w, cx);
        s.select_chat(0, w, cx);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).file_tabs.len() == 2
    })
    .await;
    cx.update(|cx| assert_eq!(shark.read(cx).tab, Tab::Chat));
    on(cx, &|s, w, cx| s.select_file(0, w, cx));
    // Restart: a second window reads the saved state.
    let (again_window, again) = open(cx, options(1440.));
    cx.wait_for(again_window, Duration::from_secs(10), |_, cx| {
        let s = again.read(cx);
        s.file_tabs.len() == 2 && s.editor.is_some()
    })
    .await;
    check(&again, cx);
    cx.update(|cx| {
        assert_eq!(again.read(cx).active_ws().unwrap().layout.ratio, Some(0.3));
    });
}

/// Opening from Diff replaces only the focused group's own file, never one shown in the
/// other group; a file that couldn't be restored closes with cmd-w instead of trapping its group.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn other_group_file_and_unavailable_file(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let mut state = fixture(&home);
    let ws = &mut state.workspaces[0];
    ws.open_tab(file("api/gone.txt"));
    ws.open_tab(file("api/later.txt"));
    assert!(ws.move_to_group(&file(A), 1, None));
    assert!(ws.move_to_group(&file("api/later.txt"), 1, None));
    assert!(ws.move_to_group(&file("api/gone.txt"), 1, None));
    ws.select_tab(&file(A));
    ws.select_tab(&file("api/gone.txt"));
    let (window, shark) = boot(cx, state, options(1440.));
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        !shark.read(cx).restore_failed.is_empty()
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let s = shark.read(cx);
        assert_eq!(s.focused_group(), 1);
        assert_eq!(s.tab, Tab::File);
        assert!(s.editor.is_none());
        assert!(
            w.try_find(("group-placeholder", 1usize)).is_some(),
            "unavailable state"
        );
    })
    .unwrap();
    press(cx, &["cmd-w"]);
    cx.update(|cx| {
        let s = shark.read(cx);
        let ws = s.active_ws().unwrap();
        assert!(!ws.tab_order.contains(&file("api/gone.txt")));
        assert_eq!(ws.group_tabs(1), [file(A), file("api/later.txt")]);
    });
    // Unavailable until now, then opened from the left: it loads in its own group and slot.
    fs::write(
        home.join("shark/workspaces/groups/api/later.txt"),
        "later\n",
    )
    .unwrap();
    on(cx, &|s, w, cx| s.show(Tab::Feature(Feature::Diff), w, cx));
    on(cx, &|s, w, cx| {
        s.open_in_group("api/later.txt".into(), "groups", 0, false, w, cx)
    });
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(groups_now(s)[1], [file(A), file("api/later.txt")]);
        assert_eq!(s.editor.as_ref().unwrap().read(cx).path, "api/later.txt");
        assert_eq!(s.focused_group(), 1);
    });
    on(cx, &|s, w, cx| s.close_file(w, cx));
    cx.update(|cx| {
        let s = shark.read(cx);
        let ws = s.active_ws().unwrap();
        assert!(ws.split(), "the split stays");
        assert_eq!(ws.group_tabs(1), [file(A)]);
        assert_eq!(s.editor.as_ref().unwrap().read(cx).path, A);
    });
    // Diff focused on the left, a.txt shown on the right: opening b from Diff selects b
    // where it is, and opening c appends it on the left; a stays.
    on(cx, &|s, w, cx| s.show(Tab::Feature(Feature::Diff), w, cx));
    cx.update(|cx| assert_eq!(shark.read(cx).focused_group(), 0));
    on(cx, &|s, w, cx| s.open_path("api/c.txt".into(), w, cx));
    let [left, right] = groups(&shark, cx);
    assert_eq!(right, [file(A)]);
    assert_eq!(left.last(), Some(&file("api/c.txt")));
    // Now the left shows c: opening d replaces it there.
    on(cx, &|s, w, cx| s.open_path("api/d.txt".into(), w, cx));
    let [left, right] = groups(&shark, cx);
    assert_eq!(right, [file(A)]);
    assert!(left.contains(&file("api/d.txt")) && !left.contains(&file("api/c.txt")));
}

/// Opt-in, real renderer (`--test rendering`): the dotted split target and its stronger hover,
/// strips aligned over their groups, the focused group's bar, in light and dark, wide and
/// narrow, with overflowing tabs.
pub(super) fn render_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let mean = |img: &Shot, b: Bounds<Pixels>, scale: f32| {
        let (x0, y0) = (
            (b.left().as_f32() * scale) as u32,
            (b.top().as_f32() * scale) as u32,
        );
        let (x1, y1) = (
            (b.right().as_f32() * scale) as u32,
            (b.bottom().as_f32() * scale) as u32,
        );
        let mut sum = [0f32; 3];
        let mut n = 0.;
        for y in y0..y1.max(y0 + 1) {
            for x in x0..x1.max(x0 + 1) {
                let p = img.px[(y * img.width + x) as usize];
                for c in 0..3 {
                    sum[c] += p[c] as f32;
                }
                n += 1.;
            }
        }
        sum.map(|s| s / n)
    };
    let distance = |a: [f32; 3], b: [f32; 3]| (0..3).map(|c| (a[c] - b[c]).abs()).sum::<f32>();
    let rgb = |h: Hsla| {
        let c = h.to_rgb();
        [c.r * 255., c.g * 255., c.b * 255.]
    };
    for (mode, mode_name) in [
        (crate::state::Mode::Dark, "dark"),
        (crate::state::Mode::Light, "light"),
    ] {
        for width in [1440., 900.] {
            let mut state = fixture(&home);
            state.appearance.mode = mode;
            // Overflowing tabs: the target still gets room.
            for n in 2..8 {
                let id = format!("groups~{n}");
                state.workspaces[0].chats.push(ChatMeta {
                    id: id.clone(),
                    title: format!("Chat {n}"),
                    ..Default::default()
                });
                state.workspaces[0].open_tab(TabId::Chat(id));
            }
            state.workspaces[0].normalize_tabs();
            state.save().unwrap();
            let (window, shark) = cx
                .update(|cx| {
                    gpui_kit::open_window(
                        WindowOptions {
                            focus: false,
                            show: false,
                            ..options(width)
                        },
                        cx,
                        |w, cx| cx.new(|cx| Shark::new(w, cx)),
                    )
                })
                .unwrap();
            cx.run_until_parked();
            let mut shots = vec![];
            for hover in [false, true] {
                let (target, strip) = cx
                    .update_window(window, |_, w, cx| {
                        w.render_frame(cx);
                        // From the tab's right end, so the preview sits left of the pointer.
                        let tab = w.find("diff-tab").bounds();
                        let start = point(tab.right() - px(4.), tab.center().y);
                        w.pointer_down(start, MouseButton::Left, cx);
                        w.pointer_move(start + point(px(8.), px(0.)), Some(MouseButton::Left), cx);
                        w.render_frame(cx);
                        let target = w.find("split-target").bounds();
                        let to = if hover {
                            target.center()
                        } else {
                            w.find("chat-tab-groups").bounds().center()
                        };
                        w.pointer_move(to, Some(MouseButton::Left), cx);
                        w.render_frame(cx);
                        (
                            w.find("split-target").bounds(),
                            w.find("chat-tabs").bounds(),
                        )
                    })
                    .unwrap();
                assert!(
                    target.left() >= strip.right(),
                    "{mode_name} {width}: the target sits after the tabs"
                );
                let shot = capture(
                    cx,
                    window,
                    &format!("groups-target-{mode_name}-{width}-{hover}"),
                );
                shots.push((shot, target));
                cx.update_window(window, |_, w, cx| {
                    shark.update(cx, |s, cx| s.cancel_drag(w, cx));
                    w.pointer_up(point(px(1.), px(400.)), MouseButton::Left, cx);
                })
                .unwrap();
            }
            let (primary, background) =
                cx.update(|cx| (rgb(cx.theme().primary), rgb(cx.theme().sidebar)));
            let scale = shots[0].0.width as f32 / width;
            let fill = |(img, b): &(Shot, Bounds<Pixels>)| {
                // Inside the dashed outline, right of the icon and the pointer's preview.
                let inner = Bounds::new(
                    point(b.right() - px(9.), b.top() + px(3.)),
                    size(px(6.), px(6.)),
                );
                mean(img, inner, scale)
            };
            let (idle, hovered) = (fill(&shots[0]), fill(&shots[1]));
            assert!(
                distance(idle, background) > 2.,
                "{mode_name} {width}: a faint accent fill {idle:?}"
            );
            assert!(
                distance(hovered, primary) < distance(idle, primary),
                "{mode_name} {width}: hover strengthens toward the accent"
            );
            // The outline is drawn: its top edge differs from the fill.
            let (img, b) = &shots[0];
            let edge = mean(
                img,
                Bounds::new(b.top_right() - point(px(10.), px(0.)), size(px(8.), px(1.))),
                scale,
            );
            assert!(
                distance(edge, idle) > 6.,
                "{mode_name} {width}: dotted outline"
            );

            // Split, then the focused group's bar and aligned strips.
            cx.update_window(window, |_, w, cx| {
                shark.update(cx, |s, cx| {
                    s.move_to_group("groups", &TabId::Diff, 1, None, w, cx)
                });
                for _ in 0..3 {
                    w.render_frame(cx);
                }
                let [left, right] = shark.read(cx).group_bounds.get();
                let (l, r) = (w.find("chat-tabs").bounds(), w.find("chat-tabs-1").bounds());
                assert!(l.right() <= left.right() + px(1.), "{mode_name} {width}");
                assert!(
                    (r.left() - right.left()).abs() < px(2.),
                    "{mode_name} {width}"
                );
            })
            .unwrap();
            let mark = cx
                .update_window(window, |_, w, _| w.find("focused-group-mark").bounds())
                .unwrap();
            let shot = capture(cx, window, &format!("groups-split-{mode_name}-{width}"));
            let bar = mean(
                &shot,
                Bounds::new(mark.center() - point(px(4.), px(0.)), size(px(8.), px(1.))),
                scale,
            );
            assert!(
                distance(bar, primary) < 60.,
                "{mode_name} {width}: the focused group's bar is the accent, {bar:?}"
            );
            cx.update_window(window, |_, w, _| w.remove_window())
                .unwrap();
        }
    }
}

/// A screenshot's pixels, saved to `SHARK_RENDER_DIR` when set.
struct Shot {
    width: u32,
    px: Vec<[u8; 4]>,
}

fn capture(cx: &mut gpui_kit::HeadlessAppContext, window: AnyWindowHandle, name: &str) -> Shot {
    let img = cx.capture_screenshot(window).expect("real renderer");
    if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
        let dir = PathBuf::from(dir);
        fs::create_dir_all(&dir).unwrap();
        img.save(dir.join(format!("{name}.png"))).unwrap();
    }
    Shot {
        width: img.width(),
        px: img.pixels().map(|p| p.0).collect(),
    }
}

fn groups_now(s: &Shark) -> [Vec<TabId>; 2] {
    let ws = s.active_ws().unwrap();
    [ws.group_tabs(0), ws.group_tabs(1)]
}
