use super::super::task_draft::BranchSpec;
use super::*;

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn add_picker_space_marks_multiple_repositories_and_enter_only_attaches(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos(&home, &["api", "web"]),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| s.enter_home(w, cx));
    press_in(window)(cx, &["cmd-i"]);
    cx.read(|cx| assert!(shark.read(cx).draft_picker.is_some()));
    press_in(window)(cx, &["space", "down", "space", "enter"]);
    until(cx, window, |_, cx| shark.read(cx).draft_picker.is_none()).await;
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(
            s.home
                .draft
                .repos
                .iter()
                .map(|r| r.repo.name.as_str())
                .collect::<Vec<_>>(),
            vec!["api", "web"]
        );
        assert!(s.home.draft.repos_edited);
        assert!(
            s.state.tasks.is_empty(),
            "context selection never starts a task"
        );
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn grouped_branch_picker_enter_checks_out_and_alt_enter_uses_base(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let registered = repos(&home, &["api"]);
    git::git(&registered[0].path, &["branch", "feature"]).unwrap();
    git::git(&registered[0].path, &["branch", "local-base"]).unwrap();
    git::git(
        &registered[0].path,
        &["update-ref", "refs/remotes/origin/feature", "main"],
    )
    .unwrap();
    let (window, shark) = boot(
        cx,
        State {
            repos: registered,
            agents: vec![crate::state::Agent {
                slug: "designer".into(),
                name: "Designer".into(),
                repos: vec!["api".into()],
                ..Default::default()
            }],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| s.prefill_agent_task("designer", w, cx));
    // Open from the branch stop so dismissal must restore the context-row focus.
    press_in(window)(cx, &["up"]);
    press_in(window)(cx, &["right", "enter"]);
    until(cx, window, |w, cx| {
        w.render_frame(cx);
        w.try_find(("source-branch", 1usize)).is_some()
    })
    .await;
    cx.update_window(window, |_, w, cx| w.input("feature", cx))
        .unwrap();
    press_in(window)(cx, &["enter"]);
    until(cx, window, |_, cx| shark.read(cx).draft_picker.is_none()).await;
    cx.update_window(window,|_,w,cx|{
        let s=shark.read(cx);
        assert!(matches!(&s.home.agent_draft.repos[0].branch,BranchSpec::Existing(name) if name=="feature"));
        assert!(s.home.tray_focus.is_focused(w),"branch selection restores its opening control");
        assert!(s.state.tasks.is_empty());
    }).unwrap();
    press_in(window)(cx, &["enter"]);
    until(cx, window, |w, cx| {
        w.render_frame(cx);
        w.try_find(("source-branch", 1usize)).is_some()
    })
    .await;
    cx.update_window(window, |_, w, cx| w.input("feature", cx))
        .unwrap();
    press_in(window)(cx, &["alt-enter"]);
    until(cx, window, |_, cx| shark.read(cx).draft_picker.is_none()).await;
    cx.read(|cx| {
        let s = shark.read(cx);
        assert!(
            matches!(&s.home.agent_draft.repos[0].branch,BranchSpec::New{base} if base=="feature")
        );
        assert!(
            s.state.tasks.is_empty(),
            "branch-from changes only draft context"
        );
    });
    press_in(window)(cx, &["enter"]);
    until(cx, window, |w, cx| {
        w.render_frame(cx);
        w.try_find(("source-branch", 2usize)).is_some()
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        w.click("draft-source-base", cx);
        w.render_frame(cx);
        w.input("local-base", cx);
    })
    .unwrap();
    press_in(window)(cx, &["enter"]);
    cx.read(|cx| {
        let s=shark.read(cx);
        assert!(matches!(&s.home.agent_draft.repos[0].branch,BranchSpec::New{base} if base=="local-base"),"the base submenu accepts a local-only branch without requiring it to be pushed");
        assert!(s.draft_picker.is_none());
        assert!(s.state.tasks.is_empty());
    });
}
