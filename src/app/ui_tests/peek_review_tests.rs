//! Regressions from the review of the task peek card (hover and Space card on task rows).
use super::*;
use crate::state::{Section, TaskRepo};
use crate::sync::{Checks, Pr, PrState};
use gpui_kit::test::TestEventExt;

#[gpui_kit::test]
fn hovered_task_opens_on_first_click_across_frames(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    cx.run_until_parked();
    on_app(window, shark.clone())(cx, &|s, w, cx| s.open_task(0, w, cx));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let target = w.within("sidebar").find(1_usize).bounds().center();
        w.pointer_move(target, None, cx);
        w.render_frame(cx);
        assert_eq!(shark.read(cx).peek.row.as_deref(), Some("two"));
        w.pointer_down(target, MouseButton::Left, cx);
        w.render_frame(cx);
        w.pointer_up(target, MouseButton::Left, cx);
        assert_eq!(shark.read(cx).active_task().unwrap().name, "two");
    })
    .unwrap();
}

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

/// Task `one` has repo `api`, checked: a PR with `checks`, and `diff` tracked lines.
fn checked(s: &mut Shark, pr: Option<Checks>, diff: Option<(u32, u32)>) {
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
    local.branch = Some("one".into());
    local.diff = diff;
    repo.pr = Some(Ok(pr.map(|checks| Pr {
        url: String::new(),
        review_checkout: false,
        number: 7,
        state: PrState::Open,
        draft: false,
        merge_state: "CLEAN".into(),
        review: String::new(),
        checks,
        head: "one".into(),
        base: "main".into(),
        change_requests: vec![],
        comments: vec![],
        contains_head: String::new(),
    })));
    s.attn.set_checked("one", vec![repo]);
}

fn row_label(cx: &mut TestAppContext, window: AnyWindowHandle, ix: usize) -> String {
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.within("sidebar")
            .find(ix)
            .label()
            .unwrap_or_default()
            .to_owned()
    })
    .unwrap()
}

/// Escape closes a card whatever has focus, and is spent doing so: the composer's own Escape
/// (to the transcript) does not also run.
#[gpui_kit::test]
fn escape_closes_peek_from_the_composer_without_its_action(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    let (window, shark) = fixture(cx);
    cx.run_until_parked();
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.open_task(0, w, cx);
        let task = s.state.tasks[0].clone();
        let chat = s.ensure_chat(&task, &task.chats[0], w, cx);
        chat.update(cx, |c, cx| c.focus_composer(w, cx));
        s.peek.task = Some("two".into());
        s.peek.keyboard = false;
        cx.notify();
    });
    // Control: with no card, Escape in the composer moves focus to the transcript.
    cx.update_window(window, |_, w, cx| {
        let s = shark.read(cx);
        let chat = s.chats.values().next().unwrap().read(cx);
        assert!(chat.composer.read(cx).focus_handle(cx).is_focused(w));
    })
    .unwrap();
    press_in(window)(cx, &["escape"]);
    cx.update_window(window, |_, w, cx| {
        let s = shark.read(cx);
        assert!(s.peek.task.is_none(), "Escape closes the card");
        let chat = s.chats.values().next().unwrap().read(cx);
        assert!(
            chat.composer.read(cx).focus_handle(cx).is_focused(w),
            "and nothing else: the composer keeps focus"
        );
        assert!(!chat.focus.is_focused(w));
    })
    .unwrap();
    // The next Escape is the composer's again.
    press_in(window)(cx, &["escape"]);
    cx.update_window(window, |_, w, cx| {
        let s = shark.read(cx);
        assert!(
            s.chats
                .values()
                .next()
                .unwrap()
                .read(cx)
                .focus
                .is_focused(w)
        );
    })
    .unwrap();
}

/// A card opened from the sidebar and left over a click elsewhere still closes on Escape.
#[gpui_kit::test]
fn escape_closes_keyboard_peek_after_focus_moves_away(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    let (window, shark) = fixture(cx);
    cx.run_until_parked();
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.open_task(1, w, cx);
        s.sel = SideRow::Task(0);
        s.sidebar_focus.focus(w, cx);
    });
    press_in(window)(cx, &["space"]);
    cx.read(|cx| assert_eq!(shark.read(cx).peek.task.as_deref(), Some("one")));
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        let task = s.state.tasks[1].clone();
        let chat = s.ensure_chat(&task, &task.chats[0], w, cx);
        chat.update(cx, |c, cx| c.focus_composer(w, cx));
    });
    press_in(window)(cx, &["escape"]);
    cx.read(|cx| assert!(shark.read(cx).peek.task.is_none()));
}

/// The card's only focusable control is Regenerate title. Closing the card while it has focus
/// leaves focus on the list the card came from, so arrows and Space keep working.
#[gpui_kit::test]
fn closing_peek_from_regenerate_button_restores_list_focus(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    cx.run_until_parked();
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.openrouter_key = Some(None);
        s.sel = SideRow::Task(0);
        s.sidebar_focus.focus(w, cx);
    });
    let focus_button = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, w, cx| {
            for _ in 0..40 {
                w.render_frame(cx);
                if w.find("regenerate-peek-title").focused() == Some(true) {
                    break;
                }
                w.focus_next(cx);
            }
            assert_eq!(w.find("regenerate-peek-title").focused(), Some(true));
        })
        .unwrap();
    };
    for close in ["escape", "pointer leaves"] {
        press_in(window)(cx, &["space"]);
        focus_button(cx);
        if close == "escape" {
            press_in(window)(cx, &["escape"]);
        } else {
            on_app(window, shark.clone())(cx, &|s, _, cx| {
                s.peek.keyboard = false;
                s.peek_leave(cx);
            });
            cx.executor().advance_clock(Duration::from_millis(250));
            cx.run_until_parked();
        }
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            assert!(shark.read(cx).peek.task.is_none(), "{close}: closed");
            let s = shark.read(cx);
            assert!(
                s.sidebar_focus.is_focused(w),
                "{close}: focus is back on the sidebar (home list: {}, home shown: {}, card: {})",
                s.home.focus.is_focused(w),
                s.home_shown(),
                s.peek
                    .focus
                    .as_ref()
                    .is_some_and(|f| f.contains_focused(w, cx)),
            );
        })
        .unwrap();
        // And it works: an arrow moves the cursor.
        press_in(window)(cx, &["down", "up"]);
        cx.read(|cx| assert_eq!(shark.read(cx).sel, SideRow::Task(0), "{close}"));
    }
}

/// The same from Home, where the card belongs to the task list.
#[gpui_kit::test]
fn closing_home_peek_from_regenerate_button_restores_list_focus(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    cx.run_until_parked();
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.openrouter_key = Some(None);
        s.enter_home(w, cx);
        s.home.sel = Some("one".into());
        s.home.focus.focus(w, cx);
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
    })
    .unwrap();
    press_in(window)(cx, &["escape"]);
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(shark.read(cx).peek.task.is_none());
        assert!(shark.read(cx).home_shown(), "Escape only closed the card");
        assert!(shark.read(cx).home.focus.is_focused(w));
    })
    .unwrap();
}

/// A failing check reads as words, not as Rust's Debug output, and an empty part leaves no
/// dangling separator.
#[gpui_kit::test]
fn sidebar_row_label_is_plain_words_without_blank_parts(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    cx.run_until_parked();
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        checked(
            s,
            Some(Checks::Failing(vec!["tests".into(), "lint".into()])),
            None,
        );
        cx.notify();
    });
    let label = row_label(cx, window, 0);
    assert!(label.contains("Checks failed: tests, lint"), "{label}");
    assert!(
        !label.contains("Failing") && !label.contains('['),
        "{label}"
    );
    assert!(!label.contains("\""), "{label}");
    // Task one has no age yet and task two no code facts: neither leaves an empty part.
    for ix in 0..2 {
        let label = row_label(cx, window, ix);
        assert!(
            label.split(" · ").all(|part| !part.trim().is_empty()),
            "blank part in {label:?}"
        );
        assert!(
            !label.trim_end().ends_with('·'),
            "dangling separator: {label:?}"
        );
    }
}

/// Diff counts on the row and card are tracked files only, and say so.
#[gpui_kit::test]
fn diff_counts_are_labelled_tracked(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    cx.run_until_parked();
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        checked(s, None, Some((12, 3)));
        s.peek.task = Some("one".into());
        s.peek.keyboard = true;
        cx.notify();
    });
    let label = row_label(cx, window, 0);
    assert!(label.contains("+12 −3 tracked"), "{label}");
    cx.update_window(window, |_, w, _| {
        let card = w.find("peek-diff-api").label().unwrap().to_owned();
        assert!(card.contains("+12 −3 tracked"), "{card}");
    })
    .unwrap();
}

/// Arrowing from a task with the card open onto a section header and on to a task keeps
/// browsing: the card follows to the next task.
#[gpui_kit::test]
fn arrows_keep_browsing_across_section_headers(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let tasks = ["one", "two"]
        .map(|name| git::create_empty_task(&tasks_dir().join(name), name, name, None).unwrap())
        .to_vec();
    let section = |name: &str, task: &str| Section {
        name: name.into(),
        icon: "folder".into(),
        collapsed: false,
        tasks: vec![task.into()],
    };
    let (window, shark) = boot(
        cx,
        State {
            sections: vec![section("A", "one"), section("B", "two")],
            tasks,
            ..Default::default()
        },
        WindowOptions::default(),
    );
    cx.run_until_parked();
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        assert_eq!(
            s.side_rows(false),
            [
                SideRow::Section(0),
                SideRow::Task(0),
                SideRow::Section(1),
                SideRow::Task(1)
            ]
        );
        s.sel = SideRow::Task(0);
        s.sidebar_focus.focus(w, cx);
    });
    press_in(window)(cx, &["space"]);
    cx.read(|cx| assert_eq!(shark.read(cx).peek.task.as_deref(), Some("one")));
    press_in(window)(cx, &["down"]);
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.sel, SideRow::Section(1));
        assert!(s.peek.task.is_none(), "no card on a header");
    });
    press_in(window)(cx, &["down"]);
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.sel, SideRow::Task(1));
        assert_eq!(s.peek.task.as_deref(), Some("two"), "card follows again");
    });
    // Escape ends browsing for good: the next header and task show nothing.
    press_in(window)(cx, &["escape", "up", "down"]);
    cx.read(|cx| assert!(shark.read(cx).peek.task.is_none()));
}

/// Closing or following the card by keyboard leaves the row under the pointer highlighted.
#[gpui_kit::test]
fn closing_peek_keeps_the_hovered_row(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    cx.run_until_parked();
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.sel = SideRow::Task(0);
        s.sidebar_focus.focus(w, cx);
        s.peek.row = Some("two".into());
        cx.notify();
    });
    press_in(window)(cx, &["space", "down"]);
    cx.read(|cx| {
        let s = shark.read(cx);
        assert!(s.peek.task.is_some());
        assert_eq!(s.peek.row.as_deref(), Some("two"), "following the card");
    });
    press_in(window)(cx, &["escape"]);
    cx.read(|cx| {
        let s = shark.read(cx);
        assert!(s.peek.task.is_none());
        assert_eq!(s.peek.row.as_deref(), Some("two"), "closing the card");
    });
    // The card itself is gone, so it can't be under the pointer.
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        s.peek.card = true;
        s.close_peek(cx);
        assert!(!s.peek.card);
    });
}

/// A chat that is still working is not "Plan to approve" even with a plan on screen.
#[gpui_kit::test]
fn busy_chat_has_no_plan_to_approve_on_the_card(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    let (window, shark) = fixture(cx);
    cx.run_until_parked();
    on_app(window, shark)(cx, &|s, w, cx| {
        let task = s.state.tasks[0].clone();
        let chat = s.ensure_chat(&task, &task.chats[0], w, cx);
        chat.update(cx, |c, cx| {
            c.plan.update(cx, |p, cx| {
                p.add("plan".into(), "# Plan".into(), cx);
            });
        });
        assert_eq!(
            super::super::peek::chat_state(Some(chat.read(cx)), cx),
            "Plan to approve"
        );
        chat.update(cx, |c, _| c.busy = true);
        let busy = super::super::peek::chat_state(Some(chat.read(cx)), cx);
        assert_ne!(busy, "Plan to approve");
        assert_eq!(busy, "Thinking");
        chat.update(cx, |c, _| c.busy = false);
    });
}
