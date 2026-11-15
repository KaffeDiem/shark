//! A new task's branch says what it is about: a Linear issue's id and first words, or the title
//! OpenRouter gave the prompt. A sea name follows only when the plain name is taken.
use super::super::task_draft::{BranchSpec, DraftRepo, TaskDraft};
use super::*;

fn draft(s: &Shark, issue: Option<(&str, &str)>) -> TaskDraft {
    let snapshot = |(identifier, title): (&str, &str)| crate::linear::Snapshot {
        id: format!("uuid-{identifier}"),
        identifier: identifier.into(),
        title: title.into(),
        ..Default::default()
    };
    TaskDraft {
        repos: vec![DraftRepo {
            repo: s.state.repos[0].clone(),
            branch: BranchSpec::New {
                base: String::new(),
            },
            pr_label: None,
        }],
        issues: issue.map(snapshot).into_iter().collect(),
        ..Default::default()
    }
}

/// Creates a task from `issue`'s draft, titled `generated`, and returns it.
async fn create(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    shark: &Entity<Shark>,
    issue: Option<(&str, &str)>,
    generated: Option<&str>,
) -> Task {
    let before = cx.read(|cx| shark.read(cx).state.tasks.len());
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        let d = draft(s, issue);
        let done: super::super::task_draft::Finished = Rc::new(|_, _, _, _| {});
        s.create_task_draft(d, None, done, generated.map(String::from), w, cx)
    });
    until(cx, window, |_, cx| {
        shark.read(cx).state.tasks.len() > before
    })
    .await;
    cx.read(|cx| shark.read(cx).state.tasks.last().cloned().unwrap())
}

fn booted(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Shark>, std::path::PathBuf) {
    let home = test_home();
    let state = State {
        repos: repos(&home, &["api"]),
        ..Default::default()
    };
    let (window, shark) = boot(cx, state, WindowOptions::default());
    (window, shark, home.join("src").join("api"))
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn a_linear_task_branch_has_the_issue_id_and_first_words(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark, _) = booted(cx);
    let issue = Some(("NG-5773", "Report tasks on the flat project overview"));
    let first = create(cx, window, &shark, issue, None).await;
    assert_eq!(first.branch, "ng-5773-report-tasks-on-the-flat-project");
    assert_eq!(first.name, first.branch);
    // The same issue again: the plain name is taken, so a sea name tells them apart.
    let again = create(cx, window, &shark, issue, None).await;
    let rest = again
        .branch
        .strip_prefix("ng-5773-report-tasks-on-the-flat-project-");
    assert!(rest.is_some_and(|sea| !sea.is_empty()), "{}", again.branch);
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn an_openrouter_title_names_the_branch_and_the_task(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark, src) = booted(cx);
    let task = create(cx, window, &shark, None, Some("Fix stale PR stacks")).await;
    assert_eq!(task.branch, "fix-stale-pr-stacks");
    assert_eq!(task.title, "Fix stale PR stacks");
    assert_eq!(
        task.chats[0].title, "Fix stale PR stacks",
        "its first tab too"
    );
    // A branch of that name in the repo already: the new one doesn't collide with it.
    git::git(&src, &["branch", "fix-login"]).unwrap();
    let task = create(cx, window, &shark, None, Some("Fix login")).await;
    assert!(task.branch.starts_with("fix-login-"), "{}", task.branch);
    // Or only on origin.
    git::git(&src, &["update-ref", "refs/remotes/origin/fix-api", "HEAD"]).unwrap();
    let task = create(cx, window, &shark, None, Some("Fix api")).await;
    assert!(task.branch.starts_with("fix-api-"), "{}", task.branch);
}
