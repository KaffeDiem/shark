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

fn ix(s: &Shark, name: &str) -> usize {
    s.state.tasks.iter().position(|t| t.name == name).unwrap()
}

fn rows(s: &Shark) -> Vec<SideRow> {
    s.side_rows(false)
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
            .insert("api".into(), vec![pr(9, "polish", "wiring")]);
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
            .insert("api".into(), vec![pr(9, "polish", "wiring")]);
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
    on_app(window, shark.clone())(cx, &|s, _, _| {
        s.state.trunks.clear();
        let t = s.state.tasks[3].clone();
        let mut repo = crate::sync::Repo::fresh(&t.repos[0], &t);
        let prs: HashMap<_, _> = [("api".to_string(), vec![pr(9, "polish", "wiring")])].into();
        assert!(
            !s.learn_stacks(&[repo.clone()], &HashMap::new()),
            "no trunk yet"
        );
        repo.local.as_mut().unwrap().trunk = "origin/main".into();
        assert!(s.learn_stacks(&[repo.clone()], &prs));
        assert!(!s.learn_stacks(&[repo], &prs));
        assert_eq!(s.state.trunks["api"], "origin/main");
        assert_eq!(State::load().trunks["api"], "origin/main", "saved");
        assert_eq!(rows(s)[2], Pr(3, 9));
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
                            .insert("api".into(), vec![pr(9, "polish", "wiring")]);
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
            .insert("api".into(), vec![pr(9, "polish", "top")]);
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
