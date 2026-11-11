use super::*;
use crate::state::{AttachStage, Attachment, TaskRepo};

fn fixture(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Shark>) {
    test_home();
    let tasks = ["one", "two"]
        .map(|name| git::create_empty_task(&tasks_dir().join(name), name, name, None).unwrap())
        .to_vec();
    boot(
        cx,
        State {
            tasks,
            ..Default::default()
        },
        WindowOptions::default(),
    )
}

#[gpui_kit::test]
fn sidebar_facts_follow_priority(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    cx.run_until_parked();
    on_app(window, shark)(cx, &|s, w, cx| {
        let mut task = s.state.tasks[0].clone();
        assert_eq!(s.task_facts(&task, cx).line, "No repository");
        task.repos.push(TaskRepo {
            name: "api".into(),
            src: PathBuf::new(),
            base: "main".into(),
            branch: None,
        });
        assert_eq!(s.task_facts(&task, cx).line, "api");
        task.issues.push(Snapshot {
            identifier: "SHA-142".into(),
            status: "In Progress".into(),
            ..Default::default()
        });
        task.linear.attach((&task.issues[0]).into());
        assert_eq!(s.task_facts(&task, cx).line, "SHA-142 · In Progress");
        task.backlog = true;
        assert_eq!(s.task_facts(&task, cx).line, "Backlog");
        task.backlog = false;
        s.unread.insert(task.chats[0].id.clone());
        assert_eq!(s.task_facts(&task, cx).line, "Agent finished");
        let chat = s.ensure_chat(&task, &task.chats[0], w, cx);
        chat.update(cx, |c, _| c.failed = Some("timeout".into()));
        assert_eq!(s.task_facts(&task, cx).line, "Turn failed · timeout");
        chat.update(cx, |c, _| {
            c.busy = true;
            c.failed = None;
        });
        assert_eq!(s.task_facts(&task, cx).line, "Agent · Thinking");
        let second = ChatMeta {
            id: "extra".into(),
            ..Default::default()
        };
        task.chats.push(second.clone());
        s.state.tasks[0].chats.push(second.clone());
        let other = s.ensure_chat(&task, &second, w, cx);
        other.update(cx, |c, _| c.busy = true);
        assert_eq!(s.task_facts(&task, cx).line, "2 chats running");
        task.attaching.push(Attachment {
            repo: "web".into(),
            src: PathBuf::new(),
            branch: "main".into(),
            base: "main".into(),
            fresh: false,
            stage: AttachStage::Queued,
            error: None,
        });
        assert_eq!(s.task_facts(&task, cx).line, "Setting up web…");
        s.home.waiting.insert(task.name.clone(), "Plan to approve");
        assert_eq!(s.task_facts(&task, cx).line, "Setting up web…");
        assert!(!s.task_facts(&task, cx).warning);
        chat.update(cx, |c, _| c.busy = false);
        other.update(cx, |c, _| c.busy = false);
        assert_eq!(s.task_facts(&task, cx).line, "Plan to approve");
        assert!(s.task_facts(&task, cx).warning);
    });
}

#[gpui_kit::test]
fn sidebar_height_and_peek_keyboard(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    cx.run_until_parked();
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.home.waiting.insert("one".into(), "Plan to approve");
        s.sidebar_focus.focus(w, cx);
        s.sel = SideRow::Task(0);
        cx.set_reduce_motion(true);
        cx.notify();
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        for ix in 0..2usize {
            assert_eq!(w.within("sidebar").find(ix).bounds().size.height, px(44.));
        }
        assert!(
            w.within("sidebar")
                .find(0usize)
                .label()
                .unwrap()
                .contains("Plan to approve")
        );
    })
    .unwrap();
    press_in(window)(cx, &["space"]);
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("task-peek").is_some());
        assert!(!shark.read(cx).peek.animate);
    })
    .unwrap();
    press_in(window)(cx, &["down"]);
    cx.read(|cx| assert_eq!(shark.read(cx).peek.task.as_deref(), Some("two")));
    press_in(window)(cx, &["escape"]);
    cx.read(|cx| assert!(shark.read(cx).peek.task.is_none()));
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.enter_home(w, cx);
        s.home.sel = Some("one".into());
        s.home.focus.focus(w, cx);
    });
    press_in(window)(cx, &["space"]);
    cx.read(|cx| {
        assert!(shark.read(cx).peek.home);
        assert_eq!(shark.read(cx).peek.task.as_deref(), Some("one"));
    });
    press_in(window)(cx, &["escape"]);
    cx.read(|cx| {
        assert!(shark.read(cx).peek.task.is_none());
        assert!(shark.read(cx).home_shown());
    });
}

#[gpui_kit::test]
fn peek_escape_is_scoped_to_its_window(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    let (other, _other_shark) = boot(cx, State::default(), WindowOptions::default());
    cx.run_until_parked();
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.sel = SideRow::Task(0);
        s.sidebar_focus.focus(w, cx);
        s.toggle_peek(w, cx);
    });
    press_in(other)(cx, &["escape"]);
    cx.read(|cx| assert_eq!(shark.read(cx).peek.task.as_deref(), Some("one")));
    press_in(window)(cx, &["escape"]);
    cx.read(|cx| assert!(shark.read(cx).peek.task.is_none()));
}

#[gpui_kit::test]
fn entering_home_from_a_sidebar_peek_focuses_home(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    cx.run_until_parked();
    on_app(window, shark)(cx, &|s, w, cx| {
        s.open_task(0, w, cx);
        s.sel = SideRow::Task(0);
        s.sidebar_focus.focus(w, cx);
        s.toggle_peek(w, cx);
        s.enter_home(w, cx);
        assert!(s.home.focus.is_focused(w));
    });
}

#[gpui_kit::test]
fn peek_hover_delay_and_card_handoff(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    cx.run_until_parked();
    on_app(window, shark.clone())(cx, &|s, _, cx| s.peek_hover("one", false, true, cx));
    cx.executor().advance_clock(Duration::from_millis(499));
    cx.run_until_parked();
    cx.read(|cx| assert!(shark.read(cx).peek.task.is_none()));
    cx.executor().advance_clock(Duration::from_millis(1));
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(shark.read(cx).peek.task.as_deref(), Some("one")));
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        s.peek_hover("one", false, false, cx);
        s.peek.card = true;
        s.peek_leave(cx);
    });
    cx.executor().advance_clock(Duration::from_millis(250));
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(shark.read(cx).peek.task.as_deref(), Some("one")));
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        s.peek.card = false;
        s.peek_hover("two", false, true, cx);
        assert_eq!(s.peek.task.as_deref(), Some("two"));
        assert!(!s.peek.animate);
        s.peek_hover("two", false, false, cx);
    });
    cx.executor().advance_clock(Duration::from_millis(200));
    cx.run_until_parked();
    cx.read(|cx| assert!(shark.read(cx).peek.task.is_none()));
}

#[gpui_kit::test]
fn chat_activity_and_turn_failure(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    let (window, shark) = fixture(cx);
    cx.run_until_parked();
    on_app(window, shark)(cx, &|s, w, cx| {
        let task = s.state.tasks[0].clone();
        let chat = s.ensure_chat(&task, &task.chats[0], w, cx);
        chat.update(cx, |c, cx| {
            use crate::harness::Ev;
            assert_eq!(c.activity(), None);
            c.busy = true;
            assert_eq!(c.activity().as_deref(), Some("Thinking"));
            c.play(Ev::TextDelta("Working".into()), cx);
            assert_eq!(c.activity().as_deref(), Some("Writing"));
            c.play(
                Ev::ToolUse {
                    id: "edit".into(),
                    name: "Edit".into(),
                    input: serde_json::json!({"file_path":"sidebar.rs"}),
                },
                cx,
            );
            assert!(c.activity().unwrap().starts_with("Edit "));
            c.play(
                Ev::Result {
                    is_error: true,
                    text: "timeout".into(),
                    secs: 1,
                },
                cx,
            );
            assert_eq!(c.activity(), None);
            assert_eq!(c.failed.as_deref(), Some("timeout"));
            assert!(c.ask("Next", w, cx));
            assert!(c.failed.is_none());
        });
    });
}

#[gpui_kit::test]
fn sidebar_completion_names_the_unread_agent(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    cx.run_until_parked();
    on_app(window, shark)(cx, &|s, _, cx| {
        s.state.agents = ["Planner", "Coder"]
            .map(|name| crate::state::Agent {
                slug: name.to_lowercase(),
                name: name.into(),
                ..Default::default()
            })
            .to_vec();
        let task = &mut s.state.tasks[0];
        task.chats[0].agent = Some("planner".into());
        task.chats.push(ChatMeta {
            id: "coder-chat".into(),
            agent: Some("coder".into()),
            ..Default::default()
        });
        s.unread.insert(task.chats[0].id.clone());
        let task = task.clone();
        assert_eq!(s.task_facts(&task, cx).line, "Planner finished");
    });
}

#[gpui_kit::test]
fn sidebar_waits_follow_each_chat_live_state(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    cx.run_until_parked();
    on_app(window, shark)(cx, &|s, w, cx| {
        let second = ChatMeta {
            id: "waiting-chat".into(),
            plan: true,
            ..Default::default()
        };
        s.state.tasks[0].chats.push(second.clone());
        let task = s.state.tasks[0].clone();
        let chat = s.ensure_chat(&task, &task.chats[0], w, cx);
        s.home
            .waiting
            .insert(task.chats[0].id.clone(), "Question to answer");
        chat.update(cx, |c, _| c.busy = true);
        assert_eq!(s.task_facts(&task, cx).line, "Agent · Thinking");
        assert!(!s.task_facts(&task, cx).warning);

        // An unloaded second chat's wait must survive the first chat resuming.
        s.home.waiting.insert(second.id.clone(), "Plan to approve");
        assert_eq!(s.task_facts(&task, cx).line, "Plan to approve");
        let other = s.ensure_chat(&task, &second, w, cx);
        other.update(cx, |c, cx| {
            c.plan
                .update(cx, |p, cx| p.add("p1".into(), "Build it".into(), cx));
        });
        assert_eq!(s.task_facts(&task, cx).line, "Plan to approve");
        other.update(cx, |c, _| c.busy = true);
        assert_eq!(s.task_facts(&task, cx).line, "2 chats running");
        assert!(!s.task_facts(&task, cx).warning);
        chat.update(cx, |c, _| c.busy = false);
        other.update(cx, |c, _| c.busy = false);
    });
}

#[gpui_kit::test]
fn keyboard_peek_cancels_pending_hover_close(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    cx.run_until_parked();
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.peek.task = Some("one".into());
        s.peek.keyboard = false;
        s.peek_leave(cx);
        s.sel = SideRow::Task(0);
        s.sidebar_focus.focus(w, cx);
    });
    press_in(window)(cx, &["down"]);
    cx.executor().advance_clock(Duration::from_millis(250));
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(shark.read(cx).peek.task.as_deref(), Some("two")));
}

#[gpui_kit::test]
fn peek_reports_actual_checkout(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    cx.run_until_parked();
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        s.state.tasks[0].repos.push(TaskRepo {
            name: "api".into(),
            src: PathBuf::new(),
            base: "main".into(),
            branch: None,
        });
        let task = &s.state.tasks[0];
        let mut repo = crate::sync::Repo::fresh(&task.repos[0], task);
        let local = repo.local.as_mut().unwrap();
        local.head = "head".into();
        local.branch = None;
        s.attn.set_checked("one", vec![repo]);
        s.peek.task = Some("one".into());
        s.peek.keyboard = true;
        cx.notify();
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(
            w.find("peek-repo-api")
                .label()
                .unwrap()
                .contains("Detached HEAD")
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn peek_regenerate_button_handles_space(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    cx.run_until_parked();
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.openrouter_key = Some(None);
        s.sel = SideRow::Task(0);
        s.sidebar_focus.focus(w, cx);
    });
    press_in(window)(cx, &["space"]);
    cx.update_window(window, |_, w, cx| {
        for _ in 0..40 {
            w.render_frame(cx);
            if w.find("regenerate-peek-title").focused() == Some(true) {
                break;
            }
            w.focus_next(cx);
        }
        assert_eq!(w.find("regenerate-peek-title").focused(), Some(true));
        let notifications = w.notifications(cx).len();
        w.press("space", cx);
        assert!(
            shark.read(cx).peek.task.is_some(),
            "Space activates the button, not the list binding"
        );
        assert_eq!(w.notifications(cx).len(), notifications + 1);
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn restored_task_still_loads_sidebar_activity(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let mut tasks = ["one", "two"]
        .map(|name| git::create_empty_task(&tasks_dir().join(name), name, name, None).unwrap())
        .to_vec();
    tasks[1].chats.push(ChatMeta {
        id: "restored-waiting-chat".into(),
        ..Default::default()
    });
    let path = crate::transcript::transcript_path("restored-waiting-chat");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, concat!(
        "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Should I proceed?\"}]}}\n",
        "{\"type\":\"result\",\"is_error\":false}\n"
    )).unwrap();
    let (window, shark) = boot(
        cx,
        State {
            tasks,
            last_open: Some(("one".into(), "chat".into())),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    until(cx, window, |_, cx| {
        shark
            .read(cx)
            .home
            .waiting
            .contains_key("restored-waiting-chat")
    })
    .await;
    cx.read(|cx| {
        let s = shark.read(cx);
        assert!(!s.home_shown());
        let facts = s.task_facts(&s.state.tasks[1], cx);
        assert_eq!(facts.line, "Question to answer");
        assert_eq!(facts.time, "now");
    });
}
