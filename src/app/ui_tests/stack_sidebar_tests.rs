//! Stacked tasks in the sidebar: one block where the bottom task sits, newest on top, a base
//! row, slim rows for PRs and Backlog tasks; sections, drags and new tasks move it whole.
use super::*;
use crate::app::sidebar::SideRow::{self, Pr, Section as Sec, Task as T};
use crate::state::{Section, TaskRepo};
use gpui_kit::test::TestEventExt as _;

fn task(name: &str, title: &str, base: &str) -> Task {
    Task {
        name: name.into(),
        title: title.into(),
        branch: name.into(),
        repos: vec![TaskRepo {
            name: "api".into(),
            src: PathBuf::from("/nonexistent/api"),
            base: base.into(),
            branch: None,
        }],
        ..Default::default()
    }
}

fn payment() -> Vec<Task> {
    vec![
        task("fix", "Fix flaky test", "origin/main"),
        task("ui", "Payment UI", "origin/api"),
        task("readme", "Update README", "origin/main"),
        task("api", "Payment API", "origin/main"),
        task("wiring", "Payment wiring", "origin/ui"),
    ]
}

fn checkout() -> Vec<Section> {
    vec![Section {
        name: "Checkout".into(),
        icon: "folder".into(),
        overview: None,
        unsent: None,
        collapsed: false,
        tasks: vec!["fix".into(), "api".into()],
    }]
}

/// Stored order fix, ui, readme, api, wiring; Checkout holds fix and api. The stack is
/// `main ← api ← ui ← wiring`, so it shows in Checkout, where api is.
fn booted(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Shark>) {
    test_home();
    let state = State {
        tasks: payment(),
        sections: checkout(),
        trunks: [("api".to_string(), "origin/main".to_string())].into(),
        ..Default::default()
    };
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.run_until_parked();
    (window, shark)
}

fn pr(number: u64, head: &str, base: &str) -> crate::prs::Pr {
    serde_json::from_value(serde_json::json!({
        "repo": "api", "number": number, "title": head, "author": null,
        "headRefName": head, "baseRefName": base,
        "url": format!("https://github.com/o/api/pull/{number}"), "state": "OPEN",
    }))
    .unwrap()
}

/// `api`'s open PRs as gh listed them at `at`.
fn listed(prs: Vec<crate::prs::Pr>, at: Instant) -> HashMap<String, crate::prs::OpenPrs> {
    let at = Some(at);
    [("api".to_string(), crate::prs::OpenPrs { at, prs })].into()
}

fn ix(s: &Shark, name: &str) -> usize {
    s.state.tasks.iter().position(|t| t.name == name).unwrap()
}

/// The sidebar's rows without the sections' Overview rows: these tests are about stacks.
fn rows(s: &Shark) -> Vec<SideRow> {
    let mut rows = s.side_rows(false);
    rows.retain(|r| !matches!(r, SideRow::Overview(_)));
    rows
}

#[gpui_kit::test]
fn a_stack_sits_where_its_bottom_task_is(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = booted(cx);
    on_app(window, shark.clone())(cx, &|s, _, _| {
        // Newest on top, in api's section; the ungrouped members don't show again below.
        assert_eq!(rows(s), vec![Sec(0), T(0), T(4), T(1), T(3), T(2)]);
        assert_eq!(s.ws_order(false), vec![0, 4, 1, 3, 2]);
        // A colleague's PR on wiring tops the stack as a cursor stop of its own.
        s.stack_prs
            .insert("api".into(), vec![pr(9, "polish", "wiring")].into());
        assert_eq!(
            rows(s),
            vec![Sec(0), T(0), Pr(3, 9), T(4), T(1), T(3), T(2)]
        );
        let url = Shark::stack_pr(&s.side_stacks(), 3, 9);
        assert_eq!(url.as_deref(), Some("https://github.com/o/api/pull/9"));
        // A Backlog member stays in the chain; cmd-N skips it.
        s.state.tasks[1].backlog = true;
        assert_eq!(
            rows(s),
            vec![Sec(0), T(0), Pr(3, 9), T(4), T(1), T(3), T(2)]
        );
        assert_eq!(s.ws_order(false), vec![0, 4, 3, 2]);
        // The rail has icons only: no PR or Backlog rows.
        s.rail = true;
        assert_eq!(rows(s), vec![Sec(0), T(0), T(4), T(3), T(2)]);
        s.rail = false;
        s.state.tasks[1].backlog = false;
        s.stack_prs.clear();
        // Folding the bottom task's section folds the whole stack.
        s.state.sections[0].collapsed = true;
        assert_eq!(rows(s), vec![Sec(0), T(2)]);
        s.state.sections[0].collapsed = false;
        // Another repository's filter shows its members as ordinary rows; with no trunk known
        // there is no stack.
        s.sidebar_repo_filter = Some(home::RepoFilter::Repo("ios".into()));
        assert_eq!(rows(s), vec![Sec(0)]);
        s.sidebar_repo_filter = None;
        s.state.trunks.clear();
        assert_eq!(rows(s), vec![Sec(0), T(0), T(3), T(1), T(2), T(4)]);
    });
}

#[gpui_kit::test]
fn sections_and_drags_move_a_stack_whole(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = booted(cx);
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        // Moving any member moves every member.
        assert_eq!(s.shown_section("wiring"), Some(0));
        s.move_to_section("wiring", None, cx);
        assert!(s.state.sections[0].tasks == ["fix"]);
        assert_eq!(rows(s), vec![Sec(0), T(0), T(2), T(4), T(1), T(3)]);
        // The stack is one slot: dropped on readme it takes readme's place, whole.
        s.drop_task("api", "readme", cx);
        let (a, r) = (ix(s, "api"), ix(s, "readme"));
        let (w, u) = (ix(s, "wiring"), ix(s, "ui"));
        assert_eq!(rows(s), vec![Sec(0), T(0), T(w), T(u), T(a), T(r)]);
        // Into a section: every member follows the bottom task there.
        s.drop_task("api", "fix", cx);
        let mut held = s.state.sections[0].tasks.clone();
        held.sort();
        assert_eq!(held, ["api", "fix", "ui", "wiring"]);
        assert_eq!(rows(s)[1..4], [T(w), T(u), T(a)]);
        // A drag can't land inside a stack: only its bottom task is a drop target.
        let names: Vec<_> = (s.side_rows(false).into_iter())
            .filter_map(|r| match r {
                T(i) => Some(i),
                _ => None,
            })
            .filter(|&i| s.stack_mates(&s.state.tasks[i].name)[0] != s.state.tasks[i].name)
            .collect();
        assert_eq!(names, [w, u]);
    });
}

/// New task on top lands in the stack and its section; Insert task below takes the old
/// bottom's slot, so nothing else moves.
#[gpui_kit::test]
fn new_stacked_tasks_join_the_stack_in_place(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = booted(cx);
    on_app(window, shark.clone())(cx, &|s, _, _| {
        s.state
            .tasks
            .push(task("top", "Payment emails", "origin/wiring"));
        s.settle_in_stack("top");
        assert!(s.state.sections[0].tasks.contains(&"top".to_string()));
        let top = ix(s, "top");
        assert_eq!(rows(s)[..3], [Sec(0), T(0), T(top)]);
        // Insert below api: api is rebased onto the new task first.
        s.state.tasks[3].repos[0].base = "origin/models".into();
        s.state
            .tasks
            .push(task("models", "Payment models", "origin/main"));
        s.settle_in_stack("models");
        assert_eq!(s.state.sections[0].tasks[..3], ["fix", "models", "api"]);
        let (m, r) = (ix(s, "models"), ix(s, "readme"));
        let r_rows = rows(s);
        assert_eq!(r_rows[r_rows.len() - 2..], [T(m), T(r)]);
        // A task in no stack is left where it was.
        s.state.tasks.push(task("solo", "Solo", "origin/main"));
        let before = s.state.sections.clone();
        s.settle_in_stack("solo");
        assert_eq!(s.state.sections, before);
    });
}

#[gpui_kit::test]
fn stack_rows_render_rail_base_and_labels(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = booted(cx);
    on_app(window, shark.clone())(cx, &|s, _, _| {
        s.stack_prs
            .insert("api".into(), vec![pr(9, "polish", "wiring")].into());
        s.state.tasks[1].backlog = true;
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let base = w.find("stack-base-api");
        assert_eq!(base.label(), Some("Stack base, main"));
        let ui = w
            .find(("stack-backlog", 1usize))
            .label()
            .unwrap()
            .to_string();
        assert!(
            ui.starts_with("Payment UI · stack 2 of 4, on Payment API"),
            "{ui}"
        );
        let wiring = w
            .within("sidebar")
            .find(4usize)
            .label()
            .unwrap()
            .to_string();
        assert!(
            wiring.starts_with("Payment wiring · stack 3 of 4, on Payment UI"),
            "{wiring}"
        );
        let api = w
            .within("sidebar")
            .find(3usize)
            .label()
            .unwrap()
            .to_string();
        assert!(
            api.starts_with("Payment API · stack 1 of 4, on main"),
            "{api}"
        );
        let pr = w.find(("stack-pr", 9usize)).label().unwrap().to_string();
        assert_eq!(pr, "#9 polish · Open · stack 4 of 4");
        // Base row under the bottom task; the PR row above the top one.
        let (api_b, base_b) = (w.within("sidebar").find(3usize).bounds(), base.bounds());
        assert_eq!(base_b.top() - api_b.bottom(), px(2.));
        assert_eq!(base_b.size.height, px(24.));
        let pr_b = w.find(("stack-pr", 9usize)).bounds();
        assert!(pr_b.bottom() < w.within("sidebar").find(4usize).bounds().top());
        // A plain task gets no stack words.
        let fix = w
            .within("sidebar")
            .find(0usize)
            .label()
            .unwrap()
            .to_string();
        assert!(!fix.contains("stack"), "{fix}");
    })
    .unwrap();
}

/// The fact line says what a stacked task needs, in the git button's words, below needs-you
/// and running; a closed parent warns.
#[gpui_kit::test]
fn stacked_facts_name_the_parent(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = booted(cx);
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        let ui = s.state.tasks[1].clone();
        let mut repo = crate::sync::Repo::fresh(&ui.repos[0], &ui);
        let local = repo.local.as_mut().unwrap();
        local.trunk = "origin/main".into();
        local.branch = Some("ui".into());
        local.base = (0, 2);
        s.attn.set_checked("ui", vec![repo.clone()]);
        assert_eq!(s.task_facts(&ui, cx).line, "Rebase onto api");
        assert!(!s.task_facts(&ui, cx).caution);
        let parent = |state: &str| {
            crate::sync::parse_prs(&format!(
                r#"[{{"number":12,"url":"u","state":"{state}","headRefOid":"p","baseRefName":"main"}}]"#
            ))
        };
        repo.local.as_mut().unwrap().base = (0, 0);
        repo.parent = parent("MERGED");
        s.attn.set_checked("ui", vec![repo.clone()]);
        assert_eq!(
            s.task_facts(&ui, cx).line,
            "Parent merged · rebase onto main"
        );
        repo.parent = parent("CLOSED");
        s.attn.set_checked("ui", vec![repo.clone()]);
        let facts = s.task_facts(&ui, cx);
        assert_eq!(
            (facts.line.as_str(), facts.caution, facts.warning),
            ("Parent PR closed", true, false)
        );
        // In sync: back to the repository line.
        repo.parent = None;
        s.attn.set_checked("ui", vec![repo]);
        assert_eq!(s.task_facts(&ui, cx).line, "api");
    });
}

/// Probes teach the sidebar each repo's trunk (saved) and open PRs, once.
#[gpui_kit::test]
fn probes_teach_trunks_and_prs(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = booted(cx);
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        s.state.trunks.clear();
        let t = s.state.tasks[3].clone();
        let mut repo = crate::sync::Repo::fresh(&t.repos[0], &t);
        let prs = listed(vec![pr(9, "polish", "wiring")], Instant::now());
        assert!(
            !s.learn_stacks(&[repo.clone()], &HashMap::new(), cx),
            "no trunk yet"
        );
        repo.local.as_mut().unwrap().trunk = "origin/main".into();
        assert!(s.learn_stacks(&[repo.clone()], &prs, cx));
        assert!(!s.learn_stacks(&[repo], &prs, cx));
        assert_eq!(s.state.trunks["api"], "origin/main");
        assert_eq!(State::load().trunks["api"], "origin/main", "saved");
        assert_eq!(rows(s)[2], Pr(3, 9));
    });
}

/// A task whose branch origin knows by another name is that branch in its stack: the name is
/// learnt from a probe, saved, and forgotten once the branch goes by its own again.
#[gpui_kit::test]
fn a_branch_origin_knows_by_another_name_keeps_its_stack(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = booted(cx);
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        // `ui` was pushed as `kasper/ui`: its PR and wiring's base say so.
        let wiring = ix(s, "wiring");
        s.state.tasks[wiring].repos[0].base = "origin/kasper/ui".into();
        let prs = vec![pr(5, "kasper/ui", "api"), pr(9, "polish", "wiring")];
        s.stack_prs.insert("api".into(), prs.into());
        let whole = vec![Sec(0), T(0), Pr(3, 9), T(4), T(1), T(3), T(2)];
        assert!(s.state.origin_branches.is_empty());
        assert_ne!(rows(s), whole, "`ui` isn't known as `kasper/ui` yet");
        let t = s.state.tasks[ix(s, "ui")].clone();
        let mut repo = crate::sync::Repo::fresh(&t.repos[0], &t);
        let none = HashMap::new();
        assert!(!s.learn_stacks(&[repo.clone()], &none, cx), "not probed");
        repo.local.as_mut().unwrap().origin_branch = "kasper/ui".into();
        assert!(s.learn_stacks(&[repo.clone()], &none, cx));
        assert!(!s.learn_stacks(&[repo.clone()], &none, cx));
        assert_eq!(rows(s), whole);
        // Cut from `ui` before it was pushed, wiring's base is its local name: still on it.
        s.state.tasks[wiring].repos[0].base = "ui".into();
        assert_eq!(rows(s), whole);
        assert_eq!(
            State::load().origin_branches["api"]["ui"],
            "kasper/ui",
            "saved"
        );
        repo.local.as_mut().unwrap().origin_branch = "ui".into();
        assert!(s.learn_stacks(&[repo], &none, cx));
        assert!(State::load().origin_branches.is_empty(), "forgotten");
    });
}

/// A stack on the real renderer, expanded and in the rail, dark and light: the rail is drawn
/// between the icons, brighter below the open task. `SHARK_RENDER_STACKS_ONLY=1` runs just
/// this; `SHARK_RENDER_DIR` keeps the screenshots.
pub(super) fn render_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    for (mode, mode_name) in [
        (crate::state::Mode::Dark, "dark"),
        (crate::state::Mode::Light, "light"),
    ] {
        let mut state = State::default();
        state.appearance.mode = mode;
        state.save().unwrap();
        let (window, shark) = cx
            .update(|cx| {
                gpui_kit::open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            point(px(0.), px(0.)),
                            size(px(900.), px(560.)),
                        ))),
                        focus: false,
                        show: false,
                        ..Default::default()
                    },
                    cx,
                    |w, cx| cx.new(|cx| Shark::new(w, cx)),
                )
            })
            .unwrap();
        for rail in [false, true] {
            let rail_x = cx
                .update_window(window, |_, w, cx| {
                    shark.update(cx, |s, cx| {
                        s.state.tasks = payment();
                        s.state.sections = checkout();
                        s.state.trunks = [("api".into(), "origin/main".into())].into();
                        s.stack_prs
                            .insert("api".into(), vec![pr(9, "polish", "wiring")].into());
                        // wiring is behind ui: the segment under it is dashed.
                        let wiring = s.state.tasks[4].clone();
                        let mut repo = crate::sync::Repo::fresh(&wiring.repos[0], &wiring);
                        let l = repo.local.as_mut().unwrap();
                        (l.trunk, l.base) = ("origin/main".into(), (0, 3));
                        s.attn.set_checked("wiring", vec![repo]);
                        s.active = Some("ui".into());
                        s.home.on = false;
                        s.sel = SideRow::Task(1);
                        s.rail = rail;
                        cx.notify();
                    });
                    w.pointer_move(point(px(899.), px(559.)), None, cx);
                    w.render_frame(cx);
                    let api = w.within("sidebar").find(3usize).bounds();
                    let wiring = w.within("sidebar").find(4usize).bounds();
                    let ui = w.within("sidebar").find(1usize).bounds();
                    let theme = cx.theme();
                    // Between wiring and ui (above the open task): dim; ui to api: bright.
                    let x = if rail { px(36.) } else { api.left() + px(16.) };
                    let gap = |a: Bounds<Pixels>, b: Bounds<Pixels>| {
                        if rail {
                            (a.bottom() + b.top()) / 2.
                        } else {
                            (a.top() + px(27.) + b.top() + px(3.)) / 2.
                        }
                    };
                    (
                        x,
                        gap(wiring, ui),
                        gap(ui, api),
                        theme.sidebar_foreground,
                        theme.foreground,
                        theme.sidebar,
                    )
                })
                .unwrap();
            let shot = cx.capture_screenshot(window).expect("real renderer");
            let name = format!(
                "stacks-{mode_name}-{}",
                if rail { "rail" } else { "expanded" }
            );
            if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
                let dir = PathBuf::from(dir);
                fs::create_dir_all(&dir).unwrap();
                shot.save(dir.join(format!("{name}.png"))).unwrap();
            }
            let (x, dim_y, bright_y, dim, bright, bg) = rail_x;
            let scale = shot.width() as f32 / 900.;
            // The darkest-to-lightest ink across the rail's two device pixels at that height.
            let ink = |y: Pixels| {
                let (px_x, px_y) = ((x.as_f32() * scale) as u32, (y.as_f32() * scale) as u32);
                (px_x.saturating_sub(2)..=px_x + 2)
                    .map(|xx| shot.get_pixel(xx, px_y).0)
                    .max_by_key(|p| {
                        let b = bg.to_rgb();
                        let b = [b.r, b.g, b.b].map(|c| (c * 255.).round() as u8);
                        (0..3).map(|c| p[c].abs_diff(b[c]) as u32).sum::<u32>()
                    })
                    .unwrap()
            };
            let near = |p: [u8; 4], c: Hsla| {
                let c = c.to_rgb();
                let c = [c.r, c.g, c.b].map(|v| (v * 255.).round() as u8);
                (0..3).all(|i| p[i].abs_diff(c[i]) <= 24)
            };
            // Dashed above the open task: one of a few heights hits a dash.
            let dash = (-3..=3).map(|o| ink(dim_y + px(o as f32)));
            let dash: Vec<_> = dash.collect();
            assert!(
                dash.iter().any(|p| near(*p, dim)),
                "{name}: rail above the open task {dash:?}"
            );
            let b = ink(bright_y);
            assert!(near(b, bright), "{name}: rail below the open task {b:?}");
        }
        cx.update_window(window, |_, w, _| w.remove_window())
            .unwrap();
        cx.run_until_parked();
    }
}

/// Review findings, each failing before its fix.
#[gpui_kit::test]
fn stack_regressions(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = booted(cx);
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        // A closed parent warns only on its own line, not over a failed turn.
        let ui = s.state.tasks[1].clone();
        let mut repo = crate::sync::Repo::fresh(&ui.repos[0], &ui);
        repo.local.as_mut().unwrap().trunk = "origin/main".into();
        repo.parent = crate::sync::parse_prs(
            r#"[{"number":12,"url":"u","state":"CLOSED","headRefOid":"p","baseRefName":"main"}]"#,
        );
        s.attn.set_checked("ui", vec![repo]);
        let chat = s.ensure_chat(&ui, &ui.chats[0], w, cx);
        chat.update(cx, |c, _| c.failed = Some("timeout".into()));
        let facts = s.task_facts(&ui, cx);
        assert_eq!(facts.line, "Turn failed · timeout");
        assert!(!facts.caution, "the failed line keeps its danger colour");

        // A filter on another repository doesn't stop a new task joining its stack.
        s.sidebar_repo_filter = Some(home::RepoFilter::Repo("ios".into()));
        s.state
            .tasks
            .push(task("top", "Payment emails", "origin/wiring"));
        s.settle_in_stack("top");
        assert!(s.state.sections[0].tasks.contains(&"top".to_string()));
        s.move_to_section("top", None, cx);
        assert!(
            s.state.sections[0].tasks == ["fix"],
            "moved whole while filtered"
        );
        s.move_to_section("top", Some(0), cx);
        s.sidebar_repo_filter = None;

        // In the rail a Backlog bottom task isn't drawn, but its stack still drags.
        s.state.tasks[3].backlog = true;
        s.rail = true;
        let items = s.reorder_items(false, cx);
        assert!(
            items.contains(&reorder::Item::Task("api".into())),
            "{items:?}"
        );
        s.rail = false;
        s.state.tasks[3].backlog = false;

        // A PR row under the cursor follows its stack when indices shift.
        s.stack_prs
            .insert("api".into(), vec![pr(9, "polish", "top")].into());
        s.sel = Pr(ix(s, "api"), 9);
        s.state.tasks.push(task("z", "Z", "origin/main"));
        s.drop_task("z", "readme", cx);
        s.drop_task("z", "fix", cx);
        assert_eq!(s.sel, Pr(ix(s, "api"), 9));
        s.stack_prs.clear();
    });
    // Archiving the bottom task leaves the rest of the stack where it was: in Checkout,
    // right after fix.
    let (window, shark) = booted(cx);
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.archived("api", Ok(vec![]), w, cx);
        let (fix, ui, wiring) = (ix(s, "fix"), ix(s, "ui"), ix(s, "wiring"));
        assert_eq!(
            (s.section_of("ui"), s.section_of("wiring")),
            (Some(0), Some(0))
        );
        assert_eq!(rows(s)[..4], [Sec(0), T(fix), T(wiring), T(ui)]);
    });
}

/// The keyboard reaches every row of a stack taller than the sidebar.
#[gpui_kit::test]
fn keyboard_scrolls_inside_a_tall_stack(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let tasks = (0..8)
        .map(|i| {
            let base = match i {
                0 => "origin/main".to_string(),
                _ => format!("origin/t{}", i - 1),
            };
            task(&format!("t{i}"), &format!("Task {i}"), &base)
        })
        .collect();
    let state = State {
        tasks,
        trunks: [("api".to_string(), "origin/main".to_string())].into(),
        ..Default::default()
    };
    let (window, shark) = boot(
        cx,
        state,
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(0.), px(0.)),
                size(px(900.), px(320.)),
            ))),
            ..Default::default()
        },
    );
    cx.run_until_parked();
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.home.on = false;
        s.sel = T(7);
        s.sidebar_focus.focus(w, cx);
    });
    let press = press_in(window);
    for i in (0..7).rev() {
        press(cx, &["down"]);
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.render_frame(cx);
            let view = shark.read(cx).sidebar_scroll.bounds();
            let row = w.within("sidebar").find(i).bounds();
            assert!(
                row.top() >= view.top() && row.bottom() <= view.bottom(),
                "t{i} at {row:?} outside {view:?}"
            );
        })
        .unwrap();
        cx.update(|cx| assert_eq!(shark.read(cx).sel, T(i)));
    }
}

/// GitHub's stack `dev ← tiger-shark ← anchor ← tab-view`, where tab-view's task was cut from
/// tiger-shark and its PR later retargeted onto anchor on GitHub.
fn retargeted(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Shark>) {
    test_home();
    let state = State {
        tasks: vec![
            task("tiger-shark", "Flatten", "origin/dev"),
            task("anchor", "Overview tasks", "origin/tiger-shark"),
            task("tab-view", "Tab view", "origin/tiger-shark"),
        ],
        trunks: [("api".to_string(), "origin/dev".to_string())].into(),
        ..Default::default()
    };
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.run_until_parked();
    (window, shark)
}

fn github() -> Vec<crate::prs::Pr> {
    vec![
        pr(1069, "tiger-shark", "dev"),
        pr(1089, "anchor", "tiger-shark"),
        pr(1096, "tab-view", "anchor"),
    ]
}

/// `(branches, base)` of each sidebar stack.
fn shown(s: &Shark) -> Vec<(Vec<String>, String)> {
    (s.side_stacks().iter())
        .map(|st| {
            let branches = st.col.nodes.iter().map(|n| n.branch.clone()).collect();
            (branches, st.col.base.clone())
        })
        .collect()
}

fn base_of(s: &Shark, task: &str) -> String {
    s.state.tasks[ix(s, task)].repos[0].base.clone()
}

fn one_stack(branches: &[&str], base: &str) -> Vec<(Vec<String>, String)> {
    let branches = branches.iter().map(|b| b.to_string()).collect();
    vec![(branches, base.to_string())]
}

/// A PR retargeted on GitHub moves its task in the sidebar: the task follows the PR's base,
/// saved, rather than keeping the base it was cut from.
#[gpui_kit::test]
fn a_pr_retargeted_on_github_moves_its_task(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = retargeted(cx);
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        assert!(s.learn_stacks(&[], &listed(github(), Instant::now()), cx));
        assert_eq!(base_of(s, "tab-view"), "origin/anchor");
        assert_eq!(
            State::load().tasks[2].repos[0].base,
            "origin/anchor",
            "saved"
        );
        assert_eq!(
            shown(s),
            one_stack(&["tiger-shark", "anchor", "tab-view"], "dev")
        );
        // Retargeted onto the trunk (as GitHub does when the parent merges): it leaves.
        let mut moved = github();
        moved[2].base_ref_name = "dev".into();
        assert!(s.learn_stacks(&[], &listed(moved, Instant::now()), cx));
        assert_eq!(base_of(s, "tab-view"), "origin/dev");
        assert_eq!(shown(s), one_stack(&["tiger-shark", "anchor"], "dev"));
    });
}

/// A review checkout of #1089 (`pr/1089` on its private base) is #1089's row in the stack,
/// on the PR's head branch, rather than floating alone while #1089 shows again as a PR row.
#[gpui_kit::test]
fn a_review_checkout_joins_its_prs_stack(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = retargeted(cx);
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        let review = ix(s, "anchor");
        let t = &mut s.state.tasks[review];
        t.name = "pr-1089".into();
        t.branch = "pr/1089".into();
        t.repos[0].base = "refs/shark/pull/1089/base".into();
        s.learn_stacks(&[], &listed(github(), Instant::now()), cx);
        assert_eq!(
            shown(s),
            one_stack(&["tiger-shark", "anchor", "tab-view"], "dev")
        );
        let tasks = &s.side_stacks()[0].tasks;
        assert_eq!(
            tasks[1],
            Some(ix(s, "pr-1089")),
            "#1089 is the review task's row"
        );
        assert_eq!(
            base_of(s, "pr-1089"),
            "refs/shark/pull/1089/base",
            "base untouched"
        );
    });
}

/// GitHub's answers apply in the order gh gave them: an older list, or the disk cache, never
/// replaces a newer one, whichever task's GitSync reports it last.
#[gpui_kit::test]
fn an_older_pr_list_never_replaces_a_newer_one(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = retargeted(cx);
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        let old = Instant::now();
        let mut before = github();
        before[2].base_ref_name = "tiger-shark".into();
        let new = Instant::now();
        assert!(s.learn_stacks(&[], &listed(github(), new), cx));
        assert!(!s.learn_stacks(&[], &listed(before.clone(), old), cx));
        let cached: HashMap<_, _> = [("api".to_string(), before.into())].into();
        assert!(!s.learn_stacks(&[], &cached, cx));
        assert_eq!(s.stack_prs["api"].prs, github());
        assert_eq!(base_of(s, "tab-view"), "origin/anchor");
    });
}

/// A base changed in Shark waits for GitHub to retarget the PR: answers that still show the
/// old base don't undo it, and one that shows a new base is GitHub's retarget.
#[gpui_kit::test]
fn a_base_set_in_shark_survives_until_github_catches_up(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = retargeted(cx);
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        let mut before = github();
        before[2].base_ref_name = "tiger-shark".into();
        s.learn_stacks(&[], &listed(before.clone(), Instant::now()), cx);
        assert_eq!(base_of(s, "tab-view"), "origin/tiger-shark");
        // An answer gh started before the change lands after it.
        let started = Instant::now();
        let moves = [(
            "api".to_string(),
            "origin/tiger-shark".to_string(),
            "origin/dev".to_string(),
        )];
        s.set_base("tab-view", &moves, false, w, cx);
        s.learn_stacks(&[], &listed(before.clone(), started), cx);
        assert_eq!(base_of(s, "tab-view"), "origin/dev");
        s.learn_stacks(&[], &listed(before, Instant::now()), cx);
        assert_eq!(
            base_of(s, "tab-view"),
            "origin/dev",
            "the retarget is pending"
        );
        assert!(!shown(s).iter().any(|(b, _)| b.contains(&"tab-view".into())));
        s.learn_stacks(&[], &listed(github(), Instant::now()), cx);
        assert_eq!(
            base_of(s, "tab-view"),
            "origin/anchor",
            "retargeted on GitHub"
        );
    });
}

/// Only an unambiguous PR of the task's own branch moves it: not a fork's PR with the same
/// branch name, not two open PRs of one branch, not a review checkout's private base.
#[gpui_kit::test]
fn only_the_branchs_own_pr_moves_a_task(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = retargeted(cx);
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        let mut fork = github();
        fork[2].is_cross_repository = true;
        s.learn_stacks(&[], &listed(fork, Instant::now()), cx);
        assert_eq!(base_of(s, "tab-view"), "origin/tiger-shark", "a fork's PR");
        let mut twice = github();
        twice.push(pr(1100, "tab-view", "dev"));
        s.learn_stacks(&[], &listed(twice, Instant::now()), cx);
        assert_eq!(base_of(s, "tab-view"), "origin/tiger-shark", "ambiguous");
        let review = ix(s, "tab-view");
        s.state.tasks[review].repos[0].base = "refs/shark/pull/1096/base".into();
        s.learn_stacks(&[], &listed(github(), Instant::now()), cx);
        assert_eq!(base_of(s, "tab-view"), "refs/shark/pull/1096/base");
    });
}

/// The background check lists the open PRs of a task's repos when no GitSync did lately, so
/// stacks in repos no open task has follow GitHub too.
#[gpui_kit::test]
fn background_checks_list_prs_no_gitsync_listed_lately(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = retargeted(cx);
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        let now = Instant::now();
        assert!(s.stack_prs_due("api", now), "nothing listed");
        s.stack_prs.insert("api".into(), github().into());
        assert!(s.stack_prs_due("api", now), "only the disk cache");
        s.learn_stacks(&[], &listed(github(), now), cx);
        assert!(!s.stack_prs_due("api", now));
        let later = now + crate::sync::FETCH_EVERY;
        assert!(s.stack_prs_due("api", later));
    });
}

/// A retarget that failed on GitHub doesn't leave the task on a base GitHub never took: the
/// next answer is followed again.
#[gpui_kit::test]
fn a_failed_retarget_follows_github_again(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = retargeted(cx);
    let app = on_app(window, shark.clone());
    app(cx, &|s, w, cx| {
        s.learn_stacks(&[], &listed(github(), Instant::now()), cx);
        let t = &s.state.tasks[ix(s, "tab-view")];
        assert!(
            !t.dir().exists(),
            "gh can't run here, so the retarget fails"
        );
        let moves = [(
            "api".to_string(),
            "origin/anchor".to_string(),
            "origin/dev".to_string(),
        )];
        s.set_base("tab-view", &moves, false, w, cx);
    });
    cx.run_until_parked();
    app(cx, &|s, _, cx| {
        s.learn_stacks(&[], &listed(github(), Instant::now()), cx);
        assert_eq!(base_of(s, "tab-view"), "origin/anchor");
    });
}

/// Presses on the sidebar row `from` and drags it over `to`; returns the pointer.
fn begin_drag(w: &mut Window, cx: &mut App, from: ElementId, to: ElementId) -> Point<Pixels> {
    w.render_frame(cx);
    let from = w.within("sidebar").find(from).bounds().center();
    let to = w.within("sidebar").find(to).bounds().center();
    w.pointer_down(from, MouseButton::Left, cx);
    w.pointer_move(from + point(px(0.), px(8.)), Some(MouseButton::Left), cx);
    w.pointer_move(to, Some(MouseButton::Left), cx);
    w.render_frame(cx);
    to
}

/// Where the sidebar draws `ids` now.
fn bounds_of(w: &mut Window, ids: &[ElementId]) -> Vec<Bounds<Pixels>> {
    (ids.iter())
        .map(|id| w.within("sidebar").find(id.clone()).bounds())
        .collect()
}

/// Releases at `at` and returns `ids`' bounds in the preview and after the drop.
fn drop_at(
    w: &mut Window,
    cx: &mut App,
    at: Point<Pixels>,
    ids: &[ElementId],
) -> (Vec<Bounds<Pixels>>, Vec<Bounds<Pixels>>) {
    let preview = bounds_of(w, ids);
    w.pointer_up(at, MouseButton::Left, cx);
    w.render_frame(cx);
    (preview, bounds_of(w, ids))
}

/// Drags the sidebar row `from` onto `to` with motion reduced, so the preview settles at
/// once, and returns `ids`' bounds in the preview and after the release.
fn drag_and_drop(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    from: ElementId,
    to: ElementId,
    ids: &[ElementId],
) -> (Vec<Bounds<Pixels>>, Vec<Bounds<Pixels>>) {
    cx.update(|cx| cx.set_reduce_motion(true));
    cx.update_window(window, |_, w, cx| {
        let at = begin_drag(w, cx, from, to);
        drop_at(w, cx, at, ids)
    })
    .unwrap()
}

/// The preview takes a stack from where it was grabbed: grabbing a lower row of the block
/// doesn't make the dragged block jump down to that row.
#[gpui_kit::test]
fn a_stack_drag_preview_starts_where_it_was_grabbed(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = booted(cx);
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let api = ix(shark.read(cx), "api");
        let block = w.within("sidebar").find("stack-api").bounds();
        let row = w.within("sidebar").find(api).bounds();
        assert!(
            row.top() > block.top() + px(40.),
            "api is the stack's lower row"
        );
        let grab = row.center();
        w.pointer_down(grab, MouseButton::Left, cx);
        w.pointer_move(grab + point(px(0.), px(8.)), Some(MouseButton::Left), cx);
        w.render_frame(cx);
        // GPUI starts the drag where the pointer crossed its threshold: the block stays put.
        let preview = w.find("reorder-preview").bounds();
        assert_eq!(preview.top(), block.top(), "the block starts where it is");
        w.pointer_move(grab + point(px(0.), px(28.)), Some(MouseButton::Left), cx);
        w.render_frame(cx);
        let preview = w.find("reorder-preview").bounds();
        assert_eq!(
            preview.top(),
            block.top() + px(20.),
            "then follows the pointer"
        );
        assert_eq!(preview.size.height, block.size.height);
        w.pointer_up(grab + point(px(0.), px(28.)), MouseButton::Left, cx);
    })
    .unwrap();
}

/// Around a stack, every row's preview spot is where the drop puts it: nothing jumps on release.
#[gpui_kit::test]
fn stack_drag_previews_land_where_the_drop_does(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = booted(cx);
    let id = |s: &Entity<Shark>, cx: &mut TestAppContext, n: &str| {
        ElementId::from(cx.read(|cx| ix(s.read(cx), n)))
    };
    let stack = ElementId::from("stack-api");
    let (fix, readme) = (id(&shark, cx, "fix"), id(&shark, cx, "readme"));

    // A task dropped on the stack: the stack's margin moves with it.
    let ids = [stack.clone(), readme.clone()];
    let (preview, after) = drag_and_drop(cx, window, fix.clone(), stack.clone(), &ids);
    on_app(window, shark.clone())(cx, &|s, _, _| {
        assert_eq!(s.state.sections[0].tasks, ["api", "fix"]);
    });
    assert_eq!(preview, after, "task onto stack");

    // `ui` was a stack's bottom row before it joined api's: its old bounds aren't a row.
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        let ui = ix(s, "ui");
        s.state.tasks[ui].repos[0].base = "origin/main".into();
        cx.notify();
    });
    cx.update_window(window, |_, w, cx| w.render_frame(cx))
        .unwrap();
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        let ui = ix(s, "ui");
        s.state.tasks[ui].repos[0].base = "origin/api".into();
        cx.notify();
    });
    let ids = [fix.clone(), readme.clone()];
    let (preview, after) = drag_and_drop(cx, window, stack.clone(), fix.clone(), &ids);
    on_app(window, shark.clone())(cx, &|s, _, _| {
        // The stack's other tasks follow it into the section.
        assert_eq!(s.state.sections[0].tasks[..2], ["fix", "api"]);
    });
    assert_eq!(preview, after, "after a stack reshuffle");

    // The stack last in the list, dragged up: its own spacing goes with it.
    on_app(window, shark.clone())(cx, &|s, _, cx| s.move_to_section("api", None, cx));
    let fix_now = id(&shark, cx, "fix");
    let readme_now = id(&shark, cx, "readme");
    on_app(window, shark.clone())(cx, &|s, _, _| {
        assert_eq!(
            *rows(s).last().unwrap(),
            T(ix(s, "api")),
            "the stack is last"
        );
    });
    let ids = [fix_now.clone(), readme_now];
    let (preview, after) = drag_and_drop(cx, window, stack.clone(), fix_now, &ids);
    assert_eq!(preview, after, "last stack moved up");
}

/// A stack that changes mid-drag (a PR list came in) lays the preview out again, even with the
/// pointer still over the same target.
#[gpui_kit::test]
fn a_stack_changing_mid_drag_lays_the_preview_out_again(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = booted(cx);
    cx.update(|cx| cx.set_reduce_motion(true));
    let (fix, ui, readme) = cx.read(|cx| {
        let s = shark.read(cx);
        (ix(s, "fix"), ix(s, "ui"), ix(s, "readme"))
    });
    // readme, last, dragged onto fix: fix and the stack below it make room.
    let to = cx
        .update_window(window, |_, w, cx| {
            begin_drag(w, cx, readme.into(), fix.into())
        })
        .unwrap();
    // `ui` leaves api's stack: `main ← ui ← wiring` now draws between api and readme.
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        s.state.tasks[ui].repos[0].base = "origin/main".into();
        cx.notify();
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.pointer_move(to, Some(MouseButton::Left), cx);
        w.render_frame(cx);
        let api = ix(shark.read(cx), "api");
        let ids = [ElementId::from(fix), api.into(), "stack-ui".into()];
        let (preview, after) = drop_at(w, cx, to, &ids);
        assert_eq!(
            shark.read(cx).state.sections[0].tasks,
            ["readme", "fix", "api"]
        );
        assert_eq!(preview, after);
    })
    .unwrap();
}
