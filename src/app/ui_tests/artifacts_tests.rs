//! Artifacts is a task feature: one tab, persisted ordering, works without repositories.
use super::*;
use gpui_kit::test::TestEventExt;

fn fixture(home: &Path) -> State {
    let repos = repos(home, &["api"]);
    let mut state = State {
        repos: repos.clone(),
        ..Default::default()
    };
    for name in ["art-main", "art-other"] {
        let mut ws = Task {
            name: name.into(),
            branch: "main".into(),
            repos: vec![crate::state::TaskRepo {
                name: "api".into(),
                src: repos[0].path.clone(),
                base: "main".into(),
                branch: None,
            }],
            ..Default::default()
        };
        ws.migrate();
        init_repo(&ws.dir().join("api"));
        state.tasks.push(ws);
    }
    let mut bare = Task {
        name: "art-bare".into(),
        branch: "main".into(),
        ..Default::default()
    };
    bare.migrate();
    fs::create_dir_all(bare.dir()).unwrap();
    state.tasks.push(bare);
    state.last_open = Some(("art-main".into(), "chat".into()));
    state
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn artifacts_tab_launch_close_reorder_and_task_restore(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(cx, fixture(&home), WindowOptions::default());
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.find("feature-artifacts").visible());
        assert!(w.try_find("artifacts-tab").is_none());
        assert!(shark.read(cx).active_task().unwrap().artifacts_closed);
        w.click("feature-artifacts", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let feature = cx.update(|cx| shark.read(cx).active_artifacts().unwrap().clone());
    for _ in 0..2 {
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.click("feature-artifacts", cx);
            w.render_frame(cx);
            let s = shark.read(cx);
            assert_eq!(s.tab, Tab::Feature(Feature::Artifacts));
            assert_eq!(s.active_artifacts(), Some(&feature));
            assert_eq!(w.find_all("artifacts-tab").len(), 1);
            assert!(!s.active_task().unwrap().artifacts_closed);
        })
        .unwrap();
    }
    assert!(!State::load().tasks[0].artifacts_closed);
    on(cx, &|s, _, cx| {
        s.move_tab(
            "art-main",
            &TabId::Artifacts,
            &TabId::Chat("art-main".into()),
            cx,
        )
    });
    assert_eq!(State::load().tasks[0].tab_order[0], TabId::Artifacts);
    press(cx, &["cmd-w"]);
    cx.update(|cx| assert_eq!(shark.read(cx).tab, Tab::Chat));
    assert!(State::load().tasks[0].artifacts_closed);
    press(cx, &["cmd-shift-a"]);
    cx.update(|cx| {
        assert_eq!(shark.read(cx).tab, Tab::Feature(Feature::Artifacts));
        assert_eq!(shark.read(cx).active_artifacts(), Some(&feature));
    });
    on(cx, &|s, w, cx| s.open_task_named("art-other", w, cx));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(shark.read(cx).active_task().unwrap().artifacts_closed);
        assert!(w.try_find("artifacts-tab").is_none());
    })
    .unwrap();
    press(cx, &["cmd-shift-a"]);
    cx.update(|cx| assert_ne!(shark.read(cx).active_artifacts(), Some(&feature)));
    on(cx, &|s, w, cx| s.open_task_named("art-main", w, cx));
    press(cx, &["cmd-shift-a"]);
    cx.update(|cx| assert_eq!(shark.read(cx).active_artifacts(), Some(&feature)));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.pointer_move(w.find("artifacts-tab").bounds().center(), None, cx);
        w.render_frame(cx);
        w.click("close-artifacts", cx);
        assert_eq!(shark.read(cx).tab, Tab::Chat);
    })
    .unwrap();
    assert!(State::load().tasks[0].artifacts_closed);
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn artifacts_rail_keys_clamp_at_the_third_feature(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(cx, fixture(&home), WindowOptions::default());
    let press = press_in(window);
    press(cx, &["cmd-shift-d", "cmd-alt-right", "cmd-alt-right"]);
    cx.update_window(window, |_, w, cx| {
        assert_eq!(shark.read(cx).focused_pane(w, cx), Some(Pane::Features));
    })
    .unwrap();
    press(cx, &["down", "down", "down", "down"]);
    cx.update(|cx| assert_eq!(shark.read(cx).feature_sel, 2));
    press(cx, &["enter"]);
    cx.update_window(window, |_, w, cx| {
        let s = shark.read(cx);
        assert_eq!(s.tab, Tab::Feature(Feature::Artifacts));
        assert_eq!(
            s.focused_pane(w, cx),
            Some(Pane::Feature(Feature::Artifacts, FeatureFocus::Main))
        );
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn artifacts_empty_state_on_a_task_without_context(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let state = fixture(&home);
    let dir = state.tasks[0].dir();
    let context = dir.join(".context");
    let (window, _shark) = boot(cx, state, WindowOptions::default());
    let press = press_in(window);
    assert!(!context.exists());
    press(cx, &["cmd-shift-a"]);
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("artifacts-empty").is_some());
        assert!(w.try_find("artifacts-reveal").is_some());
        assert!(w.try_find("attach-repository").is_none());
    })
    .unwrap();
    // The test platform has no Finder, so check the folder step Reveal runs first.
    assert_eq!(
        crate::features::artifacts::ensure_context(&dir).unwrap(),
        context
    );
    assert!(context.is_dir());
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn task_without_repos_opens_artifacts_not_the_attach_offer(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(cx, fixture(&home), WindowOptions::default());
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    on(cx, &|s, w, cx| s.open_task_named("art-bare", w, cx));
    press(cx, &["cmd-shift-a"]);
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let s = shark.read(cx);
        assert_eq!(s.tab, Tab::Feature(Feature::Artifacts));
        assert!(s.active_artifacts().is_some());
        assert!(w.try_find("artifacts-empty").is_some());
        assert!(w.try_find("attach-repository").is_none());
    })
    .unwrap();
}

/// Writes `.context/<rel>` as if made `age` seconds ago, past the scan's settling window.
fn write_artifact(task: &Path, rel: &str, bytes: &[u8], age: u64) {
    let path = task.join(".context").join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, bytes).unwrap();
    let file = fs::File::options().write(true).open(&path).unwrap();
    file.set_modified(std::time::SystemTime::now() - Duration::from_secs(age))
        .unwrap();
}

/// A task with a plan and a screenshot, Artifacts open on it.
fn with_files(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Shark>, PathBuf) {
    open_with(
        cx,
        &[
            ("plans/a.md", b"# Plan\n\nBody\n", 20),
            ("attachments/b.png", b"not really a png", 40),
        ],
    )
}

/// A task holding `files` (rel, bytes, age in seconds), Artifacts open on it.
fn open_with(
    cx: &mut TestAppContext,
    files: &[(&str, &[u8], u64)],
) -> (AnyWindowHandle, Entity<Shark>, PathBuf) {
    let home = test_home();
    let state = fixture(&home);
    let dir = state.tasks[0].dir();
    for (rel, bytes, age) in files {
        write_artifact(&dir, rel, bytes, *age);
    }
    let (window, shark) = boot(cx, state, WindowOptions::default());
    press_in(window)(cx, &["cmd-shift-a"]);
    cx.run_until_parked();
    frame(cx, window);
    (window, shark, dir)
}

fn selected(cx: &mut TestAppContext, shark: &Entity<Shark>) -> Option<String> {
    cx.update(|cx| {
        let a = shark.read(cx).active_artifacts().unwrap();
        a.read(cx).selected().map(str::to_owned)
    })
}

/// The markdown source the preview shows, if it shows a document.
fn preview_source(cx: &mut TestAppContext, shark: &Entity<Shark>) -> Option<String> {
    use crate::features::artifacts::preview::Content;
    cx.update(|cx| {
        let a = shark.read(cx).active_artifacts().unwrap();
        match &a.read(cx).preview().read(cx).content {
            Content::Doc(d) => Some(d.read(cx).source().to_owned()),
            _ => None,
        }
    })
}

fn label(cx: &mut TestAppContext, window: AnyWindowHandle, id: &'static str) -> String {
    cx.update_window(window, |_, w, _| {
        w.find(id).label().unwrap_or_default().to_string()
    })
    .unwrap()
}

fn frame(cx: &mut TestAppContext, window: AnyWindowHandle) {
    cx.update_window(window, |_, w, cx| w.render_frame(cx))
        .unwrap();
    cx.run_until_parked();
}

fn top(cx: &mut TestAppContext, window: AnyWindowHandle, id: &'static str) -> f32 {
    cx.update_window(window, |_, w, _| w.find(id).bounds().origin.y.as_f32())
        .unwrap()
}

fn present(cx: &mut TestAppContext, window: AnyWindowHandle, id: &'static str) -> bool {
    cx.update_window(window, |_, w, _| w.try_find(id).is_some())
        .unwrap()
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn artifacts_list_groups_select_newest_and_open(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark, _dir) = open_with(
        cx,
        &[
            ("plans/a.md", b"# Plan\n\nBody\n", 20),
            ("plans/c.md", b"# Third\n\nText\n", 10),
            ("attachments/b.png", b"not really a png", 40),
        ],
    );
    let press = press_in(window);
    let (plans, c, a, attachments, b) = (
        top(cx, window, "artifacts-group-plans"),
        top(cx, window, "artifact-row-plans/c.md"),
        top(cx, window, "artifact-row-plans/a.md"),
        top(cx, window, "artifacts-group-attachments"),
        top(cx, window, "artifact-row-attachments/b.png"),
    );
    assert!(plans < c && c < a && a < attachments && attachments < b);
    // The newest file is selected on open, and its preview shows its text.
    assert_eq!(selected(cx, &shark).as_deref(), Some("plans/c.md"));
    assert!(preview_source(cx, &shark).is_some_and(|t| t.contains("# Third")));
    cx.update_window(window, |_, w, cx| w.click("artifact-row-plans/a.md", cx))
        .unwrap();
    frame(cx, window);
    frame(cx, window);
    assert_eq!(selected(cx, &shark).as_deref(), Some("plans/a.md"));
    assert!(preview_source(cx, &shark).is_some_and(|t| t.contains("# Plan")));
    press(cx, &["enter"]);
    cx.update(|cx| {
        let s = shark.read(cx);
        let editor = s.editor.as_ref().expect("a file tab opened");
        assert_eq!(editor.read(cx).path, ".context/plans/a.md");
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn artifacts_keys_move_and_delete_asks_first(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark, dir) = with_files(cx);
    let press = press_in(window);
    let (a, b) = (
        dir.join(".context/plans/a.md"),
        dir.join(".context/attachments/b.png"),
    );
    press(cx, &["down", "cmd-backspace"]);
    assert_eq!(selected(cx, &shark).as_deref(), Some("attachments/b.png"));
    cx.update_window(window, |_, w, cx| assert!(w.has_active_dialog(cx)))
        .unwrap();
    press(cx, &["escape"]);
    assert!(b.exists(), "cancel keeps the file");
    press(cx, &["cmd-backspace", "enter"]);
    frame(cx, window);
    assert!(!b.exists() && a.exists());
    assert!(!present(cx, window, "artifact-row-attachments/b.png"));
    assert!(present(cx, window, "artifact-row-plans/a.md"));
    assert_eq!(selected(cx, &shark).as_deref(), Some("plans/a.md"));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn artifacts_removed_externally_keeps_the_preview(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark, dir) = with_files(cx);
    let press = press_in(window);
    assert!(!present(cx, window, "artifact-removed"));
    fs::remove_file(dir.join(".context/plans/a.md")).unwrap();
    press(cx, &["r"]);
    frame(cx, window);
    assert!(!present(cx, window, "artifact-row-plans/a.md"));
    assert!(present(cx, window, "artifact-row-attachments/b.png"));
    assert!(present(cx, window, "artifacts-preview"));
    assert!(present(cx, window, "artifact-removed"));
    assert_eq!(selected(cx, &shark).as_deref(), Some("plans/a.md"));
    assert!(preview_source(cx, &shark).is_some_and(|t| t.contains("# Plan")));
    // Moving selects the neighbor and the note goes.
    press(cx, &["down"]);
    frame(cx, window);
    assert!(!present(cx, window, "artifact-removed"));
    assert_eq!(selected(cx, &shark).as_deref(), Some("attachments/b.png"));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn artifacts_new_file_replaces_a_ghost_of_the_only_file(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark, dir) = open_with(cx, &[("plans/a.md", b"# Plan\n", 20)]);
    let press = press_in(window);
    fs::remove_file(dir.join(".context/plans/a.md")).unwrap();
    press(cx, &["r"]);
    frame(cx, window);
    assert!(present(cx, window, "artifacts-empty"));
    assert_eq!(selected(cx, &shark), None, "nothing is left to select");
    write_artifact(&dir, "plans/n.md", b"# Fresh\n", 10);
    press(cx, &["r"]);
    frame(cx, window);
    frame(cx, window);
    assert_eq!(selected(cx, &shark).as_deref(), Some("plans/n.md"));
    assert!(!present(cx, window, "artifact-removed"));
    assert!(preview_source(cx, &shark).is_some_and(|t| t.contains("# Fresh")));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn artifacts_filter_hides_rows_and_says_when_nothing_matches(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark, _dir) = open_with(
        cx,
        &[
            ("plans/a.md", b"# Plan\n", 20),
            ("plans/c.md", b"# C\n", 10),
            ("attachments/b.png", b"not really a png", 40),
        ],
    );
    let type_in = |cx: &mut TestAppContext, text: &'static str| {
        // cmd-f focuses the box; cmd-a selects the last query so typing replaces it.
        press_in(window)(cx, &["cmd-f", "cmd-a"]);
        cx.update_window(window, |_, w, cx| w.input(text, cx))
            .unwrap();
        frame(cx, window);
    };
    assert!(present(cx, window, "artifacts-filter"));
    type_in(cx, "B.PNG");
    assert!(present(cx, window, "artifact-row-attachments/b.png"));
    assert!(!present(cx, window, "artifact-row-plans/a.md"));
    assert!(!present(cx, window, "artifacts-no-match"));
    // The selection followed the filter, so Enter opens a file that is in view.
    assert_eq!(selected(cx, &shark).as_deref(), Some("attachments/b.png"));
    type_in(cx, "c.md");
    assert!(!present(cx, window, "artifact-row-plans/a.md"));
    assert_eq!(
        label(cx, window, "artifacts-group-plans"),
        "Plans, 2 files, expanded",
        "the count stays the group's total"
    );
    type_in(cx, "query");
    assert_eq!(
        label(cx, window, "artifacts-no-match"),
        "No artifacts match \"query\""
    );
    cx.update_window(window, |_, w, cx| w.click("artifacts-clear-filter", cx))
        .unwrap();
    frame(cx, window);
    assert!(present(cx, window, "artifact-row-plans/a.md"));
    assert!(!present(cx, window, "artifacts-no-match"));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn artifacts_enter_after_filtering_opens_a_visible_file(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark, _dir) = with_files(cx);
    let press = press_in(window);
    assert_eq!(selected(cx, &shark).as_deref(), Some("plans/a.md"));
    press(cx, &["cmd-f"]);
    cx.update_window(window, |_, w, cx| w.input("b.png", cx))
        .unwrap();
    frame(cx, window);
    press(cx, &["enter"]);
    press(cx, &["enter"]);
    cx.update(|cx| {
        let editor = shark.read(cx).editor.clone().expect("a file tab opened");
        assert_eq!(editor.read(cx).path, ".context/attachments/b.png");
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn artifacts_group_of_a_multibyte_folder_renders(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, _shark, _dir) = open_with(cx, &[("été/x.md", b"# X\n", 20)]);
    assert_eq!(
        label(cx, window, "artifacts-group-été"),
        "Été, 1 file, expanded"
    );
    assert!(present(cx, window, "artifact-row-été/x.md"));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn artifacts_a_attaches_and_cmd_c_copies_the_path(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark, dir) = with_files(cx);
    let press = press_in(window);
    press(cx, &["cmd-c"]);
    let want = dir.join(".context/plans/a.md").display().to_string();
    assert_eq!(
        cx.update(|cx| cx.read_from_clipboard().and_then(|c| c.text())),
        Some(want)
    );
    press(cx, &["a"]);
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        let s = shark.read(cx);
        assert_eq!(s.tab, Tab::Chat);
        let chat = s.active_chat().unwrap().read(cx);
        let text = chat.composer.read(cx).value().to_string();
        assert!(text.contains("@.context/plans/a.md"), "{text}");
        assert!(chat.composer.read(cx).focus_handle(cx).is_focused(w));
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn artifacts_new_marker_and_rail_badge_follow_unseen_files(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, _shark, dir) = with_files(cx);
    // Files present at the first scan are not new.
    assert!(!present(cx, window, "artifact-new-plans/a.md"));
    assert!(!present(cx, window, "artifact-new-attachments/b.png"));
    assert!(!present(cx, window, "feature-artifacts-badge"));
    write_artifact(&dir, "plans/c.md", b"# C\n", 10);
    press_in(window)(cx, &["r"]);
    frame(cx, window);
    assert!(present(cx, window, "artifact-new-plans/c.md"));
    assert!(!present(cx, window, "artifact-new-plans/a.md"));
    assert!(present(cx, window, "feature-artifacts-badge"));
    // Previewing clears it.
    cx.update_window(window, |_, w, cx| w.click("artifact-row-plans/c.md", cx))
        .unwrap();
    frame(cx, window);
    frame(cx, window);
    assert!(!present(cx, window, "artifact-new-plans/c.md"));
    assert!(!present(cx, window, "feature-artifacts-badge"));
    // A changed file is new again.
    write_artifact(&dir, "plans/a.md", b"# Plan\n\nChanged\n", 5);
    press_in(window)(cx, &["r"]);
    frame(cx, window);
    assert!(present(cx, window, "artifact-new-plans/a.md"));
    assert!(!present(cx, window, "artifact-new-plans/c.md"));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn artifacts_plan_copy_shows_in_its_chat(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let mut state = fixture(&home);
    state.tasks[0].chats[0].title = "Fix login".into();
    let dir = state.tasks[0].dir();
    let chat_id = state.tasks[0].chats[0].id.clone();
    write_artifact(&dir, "plans/Fix login plan.md", b"# Plan\n", 20);
    write_artifact(&dir, "plans/other plan.md", b"# Other\n", 30);
    let (window, shark) = boot(cx, state, WindowOptions::default());
    press_in(window)(cx, &["cmd-shift-a"]);
    cx.run_until_parked();
    frame(cx, window);
    // The newest file, the plan copy, is selected.
    assert!(present(cx, window, "artifact-show-chat"));
    cx.update_window(window, |_, w, cx| {
        w.click("artifact-row-plans/other plan.md", cx)
    })
    .unwrap();
    frame(cx, window);
    frame(cx, window);
    assert!(!present(cx, window, "artifact-show-chat"));
    cx.update_window(window, |_, w, cx| {
        w.click("artifact-row-plans/Fix login plan.md", cx)
    })
    .unwrap();
    frame(cx, window);
    frame(cx, window);
    cx.update_window(window, |_, w, cx| w.click("artifact-show-chat", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.tab, Tab::Chat);
        let chat = s.active_chat().unwrap().read(cx);
        assert_eq!(chat.meta.id, chat_id);
        assert!(chat.plan_open);
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn artifacts_p_pins_to_the_top_and_survives_a_relaunch(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark, _dir) = with_files(cx);
    assert!(!present(cx, window, "artifacts-group-pinned"));
    assert_eq!(label(cx, window, "artifact-pin"), "Pin");
    press_in(window)(cx, &["p"]);
    frame(cx, window);
    frame(cx, window);
    assert_eq!(
        label(cx, window, "artifacts-group-pinned"),
        "Pinned, 1 file, expanded"
    );
    let (group, row, next) = (
        top(cx, window, "artifacts-group-pinned"),
        top(cx, window, "artifact-row-plans/a.md"),
        top(cx, window, "artifacts-group-attachments"),
    );
    assert!(group < row && row < next);
    assert!(!present(cx, window, "artifacts-group-plans"));
    assert!(label(cx, window, "artifact-row-plans/a.md").ends_with(" · pinned"));
    assert_eq!(label(cx, window, "artifact-pin"), "Unpin");
    assert_eq!(selected(cx, &shark).as_deref(), Some("plans/a.md"));
    assert_eq!(State::load().tasks[0].artifacts_pinned, ["plans/a.md"]);
    // A relaunch restores it.
    let (window, _shark) = boot(cx, State::load(), WindowOptions::default());
    press_in(window)(cx, &["cmd-shift-a"]);
    cx.run_until_parked();
    frame(cx, window);
    assert!(present(cx, window, "artifacts-group-pinned"));
    assert!(label(cx, window, "artifact-row-plans/a.md").ends_with(" · pinned"));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn artifacts_toolbar_unpins_and_context_menu_offers_it(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark, _dir) = with_files(cx);
    cx.update_window(window, |_, w, cx| w.click("artifact-pin", cx))
        .unwrap();
    frame(cx, window);
    frame(cx, window);
    assert!(present(cx, window, "artifacts-group-pinned"));
    assert_eq!(State::load().tasks[0].artifacts_pinned, ["plans/a.md"]);
    cx.update_window(window, |_, w, cx| w.click("artifact-pin", cx))
        .unwrap();
    frame(cx, window);
    frame(cx, window);
    assert!(!present(cx, window, "artifacts-group-pinned"));
    assert!(present(cx, window, "artifacts-group-plans"));
    assert!(State::load().tasks[0].artifacts_pinned.is_empty());
    assert_eq!(selected(cx, &shark).as_deref(), Some("plans/a.md"));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn artifacts_deleting_a_pinned_file_drops_its_pin(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, _shark, _dir) = with_files(cx);
    let press = press_in(window);
    press(cx, &["p"]);
    frame(cx, window);
    assert_eq!(State::load().tasks[0].artifacts_pinned, ["plans/a.md"]);
    press(cx, &["cmd-backspace", "enter"]);
    frame(cx, window);
    assert!(!present(cx, window, "artifacts-group-pinned"));
    assert!(State::load().tasks[0].artifacts_pinned.is_empty());
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn artifacts_a_removed_file_cannot_be_pinned(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark, dir) = with_files(cx);
    let press = press_in(window);
    fs::remove_file(dir.join(".context/plans/a.md")).unwrap();
    press(cx, &["r"]);
    frame(cx, window);
    assert!(present(cx, window, "artifact-removed"));
    press(cx, &["p"]);
    frame(cx, window);
    assert!(
        State::load().tasks[0].artifacts_pinned.is_empty(),
        "a pin on a hidden row could never be undone"
    );
    assert!(present(cx, window, "artifact-removed"));
    assert_eq!(selected(cx, &shark).as_deref(), Some("plans/a.md"));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn artifacts_a_new_file_the_filter_hides_is_not_selected(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark, dir) = with_files(cx);
    press_in(window)(cx, &["cmd-f"]);
    cx.update_window(window, |_, w, cx| w.input("zzz", cx))
        .unwrap();
    frame(cx, window);
    assert_eq!(selected(cx, &shark), None);
    write_artifact(&dir, "plans/n.md", b"# Fresh\n", 5);
    let artifacts = cx.update(|cx| shark.read(cx).active_artifacts().unwrap().clone());
    cx.update_window(window, |_, w, cx| {
        artifacts.update(cx, |a, cx| a.refresh(w, cx))
    })
    .unwrap();
    frame(cx, window);
    frame(cx, window);
    assert_eq!(selected(cx, &shark), None, "the filter hides plans/n.md");
    assert!(preview_source(cx, &shark).is_none());
}

fn refresh(cx: &mut TestAppContext, window: AnyWindowHandle, shark: &Entity<Shark>) {
    let a = cx.update(|cx| shark.read(cx).active_artifacts().unwrap().clone());
    cx.update_window(window, |_, w, cx| a.update(cx, |a, cx| a.refresh(w, cx)))
        .unwrap();
}

/// A scan that read the folder before a delete lands after it: the file must stay gone.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn artifacts_a_scan_started_before_a_delete_does_not_bring_it_back(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark, dir) = with_files(cx);
    press_in(window)(cx, &["down", "cmd-backspace"]);
    refresh(cx, window, &shark);
    // Start the scan and let it read the folder, but not land.
    cx.dispatcher.tick(false);
    while cx.dispatcher.tick(true) {}
    cx.update_window(window, |_, w, cx| w.press("enter", cx))
        .unwrap();
    assert!(!dir.join(".context/attachments/b.png").exists());
    cx.run_until_parked();
    frame(cx, window);
    assert!(!present(cx, window, "artifact-row-attachments/b.png"));
    assert_eq!(selected(cx, &shark).as_deref(), Some("plans/a.md"));
}

/// A turn that rewrites the previewed plan while another tab shows still counts as news.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn artifacts_a_change_while_hidden_stays_new(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark, dir) = with_files(cx);
    assert_eq!(selected(cx, &shark).as_deref(), Some("plans/a.md"));
    on_app(window, shark.clone())(cx, &|s, w, cx| s.show(Tab::Chat, w, cx));
    write_artifact(&dir, "plans/a.md", b"# Plan\n\nRewritten\n", 5);
    refresh(cx, window, &shark);
    cx.run_until_parked();
    frame(cx, window);
    assert!(present(cx, window, "feature-artifacts-badge"));
}

/// A rewritten screenshot redraws its thumbnail instead of the cached old one.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn artifacts_a_rewritten_image_drops_its_cached_thumbnail(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    const RED: &[u8] = b"\x89\x50\x4e\x47\x0d\x0a\x1a\x0a\x00\x00\x00\x0d\x49\x48\x44\x52\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00\x90\x77\x53\xde\x00\x00\x00\x0c\x49\x44\x41\x54\x78\x9c\x63\xf8\xcf\xc0\x00\x00\x03\x01\x01\x00\xc9\xfe\x92\xef\x00\x00\x00\x00\x49\x45\x4e\x44\xae\x42\x60\x82";
    const BLUE: &[u8] = b"\x89\x50\x4e\x47\x0d\x0a\x1a\x0a\x00\x00\x00\x0d\x49\x48\x44\x52\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00\x90\x77\x53\xde\x00\x00\x00\x0c\x49\x44\x41\x54\x78\x9c\x63\x60\x60\xf8\x0f\x00\x01\x03\x01\x00\x08\x89\xc2\xec\x00\x00\x00\x00\x49\x45\x4e\x44\xae\x42\x60\x82";
    let (window, shark, dir) = open_with(cx, &[("attachments/b.png", RED, 40)]);
    let res = Resource::Path(dir.join(".context/attachments/b.png").into());
    let image = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let a = shark.read(cx).active_artifacts().unwrap().read(cx);
            a.lru().read(cx).image(&res)
        })
    };
    frame(cx, window);
    let red = image(cx).expect("the list decoded its thumbnail");
    write_artifact(&dir, "attachments/b.png", BLUE, 5);
    refresh(cx, window, &shark);
    cx.run_until_parked();
    frame(cx, window);
    let now = image(cx).expect("the list decoded it again");
    assert!(!Arc::ptr_eq(&red, &now), "the thumbnail is the old decode");
}
