use super::*;
use crate::models::{MODELS, effort_label, model_label};
use crate::state::Agent;

fn agents() -> Vec<Agent> {
    vec![
        Agent {
            slug: "designer".into(),
            name: "Designer".into(),
            ..Default::default()
        },
        Agent {
            slug: "planner".into(),
            name: "Planner".into(),
            model: Some(MODELS[1].into()),
            effort: Some("low".into()),
            ..Default::default()
        },
    ]
}

/// Settings › Agents: Settings on the Agents page.
fn open_agents(cx: &mut TestAppContext, window: AnyWindowHandle, shark: &Entity<Shark>) {
    on_app(window, shark.clone())(cx, &|s, w, cx| crate::settings::open(&s.state, w, cx));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("Agents", cx);
        w.render_frame(cx);
    })
    .unwrap();
}

/// The slug the open agent dialog edits.
fn editing(shark: &Entity<Shark>, cx: &App) -> Option<String> {
    let editor = shark.read(cx).agent_editor.clone()?.upgrade()?;
    editor.read(cx).slug.clone()
}

fn row_labels(w: &mut Window, cx: &mut App) -> Vec<String> {
    w.render_frame(cx);
    (0..)
        .map_while(|ix| w.try_find(("agent-row", ix as usize)))
        .map(|r| r.label().unwrap_or_default().to_string())
        .collect()
}

/// Each row names the agent, its slug and its model · thinking; a click or Enter opens its
/// dialog over Settings, and Done returns to the list.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn agents_page_lists_agents_and_a_row_opens_its_dialog(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(
        cx,
        State {
            agents: agents(),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    open_agents(cx, window, &shark);
    let planner = format!(
        "Planner, @planner, {} · {}",
        model_label(MODELS[1]),
        effort_label("low")
    );
    cx.update_window(window, |_, w, cx| {
        assert_eq!(
            row_labels(w, cx),
            ["Designer, @designer, App default", planner.as_str()]
        );
        assert!(w.try_find("agents-empty").is_none());
        w.click(("agent-row", 1usize), cx);
        w.render_frame(cx);
        assert!(
            w.try_find("settings-page").is_some(),
            "Settings stays under"
        );
        assert!(w.try_find("agent-editor").is_some());
        assert_eq!(editing(&shark, cx).as_deref(), Some("planner"));
        w.click("agent-done", cx);
        w.render_frame(cx);
        assert!(w.try_find("agent-editor").is_none());
        assert!(w.try_find("settings-page").is_some());
    })
    .unwrap();
    // Keyboard: Tab reaches a row, Enter opens it.
    let mut reached = false;
    for _ in 0..12 {
        press_in(window)(cx, &["tab"]);
        reached = cx
            .update_window(window, |_, w, cx| {
                w.render_frame(cx);
                w.find(("agent-row", 0usize)).focused() == Some(true)
            })
            .unwrap();
        if reached {
            break;
        }
    }
    assert!(reached, "Tab reaches the first row");
    press_in(window)(cx, &["enter"]);
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("agent-editor").is_some());
        assert_eq!(editing(&shark, cx).as_deref(), Some("designer"));
    })
    .unwrap();
}

/// Deleting from a row's dialog closes the confirmation and the dialog, not Settings, and the
/// list drops the agent.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn deleting_from_agents_page_keeps_settings_open(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(
        cx,
        State {
            agents: agents(),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    open_agents(cx, window, &shark);
    cx.update_window(window, |_, w, cx| {
        w.click(("agent-row", 0usize), cx);
        w.render_frame(cx);
        w.click("agent-delete", cx);
    })
    .unwrap();
    press_in(window)(cx, &["enter"]);
    cx.read(|cx| {
        let slugs: Vec<_> = (shark.read(cx).state.agents.iter())
            .map(|a| a.slug.clone())
            .collect();
        assert_eq!(slugs, ["planner"]);
    });
    cx.update_window(window, |_, w, cx| {
        let rows = row_labels(w, cx);
        assert!(w.try_find("agent-editor").is_none(), "the dialog closes");
        assert!(w.try_find("settings-page").is_some(), "Settings stays");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].starts_with("Planner"));
    })
    .unwrap();
}

/// No agents: the empty state names New agent, which creates one into the list.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn agents_page_empty_state_and_new_agent(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    open_agents(cx, window, &shark);
    cx.update_window(window, |_, w, cx| {
        assert!(row_labels(w, cx).is_empty());
        assert!(w.try_find("agents-empty").is_some());
        w.click("new-agent", cx);
        w.render_frame(cx);
        assert!(w.try_find("agent-create").is_some());
        w.click("agent-name", cx);
        w.input("Reviewer", cx);
        w.render_frame(cx);
        w.click("agent-create", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        assert_eq!(row_labels(w, cx), ["Reviewer, @reviewer, App default"]);
        assert!(w.try_find("agent-editor").is_none());
        assert!(w.try_find("agents-empty").is_none());
        assert!(w.try_find("settings-page").is_some());
    })
    .unwrap();
}

/// The Agents page sits before Repositories; `open_repos` still lands on Repositories.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn open_repos_lands_on_repositories(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            agents: agents(),
            repos: repos(&home, &["api"]),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| crate::settings::open_repos(&s.state, w, cx));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find(("repo", 0usize)).is_some());
        assert!(w.try_find(("agent-row", 0usize)).is_none());
    })
    .unwrap();
}
