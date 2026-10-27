use std::{cell::RefCell, rc::Rc};

use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ListOffset, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Render, SharedString,
    Styled as _, TestAppContext, VisualTestContext, Window, div, point, px,
};

use super::{
    Diff, DiffEvent, DiffLinePosition, DiffMode, DiffSide, DiffState, document::fixture,
    state::DisplayRow,
};

fn lines(word: &str, count: usize) -> String {
    (1..=count).map(|ix| format!("{word} {ix}\n")).collect()
}

/// Lines 10 and 50 of 60 changed: two hunks with a fold between them.
fn two_hunks() -> super::DiffFile {
    let original = lines("line", 60);
    let modified = original
        .replace("line 10\n", "changed 10\n")
        .replace("line 50\n", "changed 50\n");
    fixture::modified("split.txt", &original, &modified)
}

#[gpui_kit::test]
fn split_ratio_is_clamped_and_silent_when_set(cx: &mut TestAppContext) {
    let state = cx.new(|cx| DiffState::new([two_hunks()], cx));
    state.update(cx, |state, cx| {
        assert_eq!(state.split_ratio(), 0.5);
        state.set_split_ratio(0.05, cx);
        assert_eq!(state.split_ratio(), 0.2);
        state.set_split_ratio(0.95, cx);
        assert_eq!(state.split_ratio(), 0.8);
        // A drag is relative to where it started and reports once, when it ends.
        state.set_split_ratio(0.5, cx);
        state.begin_divider_drag(px(100.));
        state.drag_divider(px(140.), px(400.), cx);
        assert!((state.split_ratio() - 0.6).abs() < 1e-4);
        state.drag_divider(px(400.), px(400.), cx);
        assert_eq!(state.split_ratio(), 0.8);
        state.end_divider_drag(cx);
        state.end_divider_drag(cx);
        // Without a drag, pointer moves leave the ratio alone.
        state.drag_divider(px(0.), px(400.), cx);
        assert_eq!(state.split_ratio(), 0.8);
    });
}

#[gpui_kit::test]
fn replace_file_keeps_scroll_and_shows_new_lines(cx: &mut TestAppContext) {
    let state = cx.new(|cx| {
        DiffState::new([two_hunks(), fixture::modified("b.txt", "b\n", "B\n")], cx)
            .with_mode(DiffMode::Split)
    });
    // Line 30 sits in the fold between the hunks until it is shown.
    let shown = |state: &DiffState, line: usize| {
        let line = DiffLinePosition::new("split.txt", DiffSide::Modified, line);
        state
            .source_row(&line)
            .is_some_and(|row| matches!(state.rows()[row], DisplayRow::Code { .. }))
    };
    state.update(cx, |state, cx| state.ensure_presentation(cx));
    cx.run_until_parked();
    state.update(cx, |state, cx| {
        state.set_file_collapsed("b.txt", true, cx);
        assert!(!shown(state, 30));
        state.show_line(
            DiffLinePosition::new("split.txt", DiffSide::Modified, 30),
            cx,
        );
        assert!(shown(state, 30));
        state.set_selected_lines(
            Some(super::DiffLineRange::new(
                "split.txt",
                DiffSide::Modified,
                5,
                6,
            )),
            cx,
        );
        state.list().scroll_to(ListOffset {
            item_ix: 3,
            offset_in_item: px(5.),
        });
        let original = lines("line", 60);
        let edited = original
            .replace("line 10\n", "changed 10\nadded 10\n")
            .replace("line 50\n", "changed 50\n");
        state.replace_file(fixture::modified("split.txt", &original, &edited), cx);
        let top = state.list().logical_scroll_top();
        assert_eq!((top.item_ix, top.offset_in_item), (3, px(5.)));
        // The old colors stay until new ones are ready; the scroll range is measured again.
        assert!(state.presentations()[0].is_some() && state.measure_width);
        assert!(state.is_file_collapsed("b.txt"));
        assert_eq!(state.files()[0].additions(), 3);
        // The edited file keeps its open fold (now one line lower) and its selection.
        assert!(shown(state, 31));
        assert_eq!(
            state.selected_lines(),
            Some(super::DiffLineRange::new(
                "split.txt",
                DiffSide::Modified,
                5,
                6
            ))
        );
        // A top row inside the change stays put, and the open fold moves with its lines.
        state.list().scroll_to(ListOffset {
            item_ix: 12,
            offset_in_item: px(5.),
        });
        let edited = edited.replace("added 10\n", &lines("added", 10));
        state.replace_file(fixture::modified("split.txt", &original, &edited), cx);
        let top = state.list().logical_scroll_top();
        assert_eq!((top.item_ix, top.offset_in_item), (12, px(5.)));
        assert!(shown(state, 55));
        // Unknown paths are ignored.
        state.replace_file(fixture::modified("missing.txt", "a\n", "b\n"), cx);
        assert_eq!(state.files().len(), 2);
    });
}

struct Host {
    state: Entity<DiffState>,
    calls: Rc<RefCell<Vec<(SharedString, usize)>>>,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let calls = self.calls.clone();
        div().w(px(400.)).h(px(300.)).child(
            Diff::new(&self.state)
                .size_full()
                .side_headings(|file, _, _| {
                    (
                        format!("Original {}", file.path()).into(),
                        "Working copy".into(),
                    )
                })
                .render_working_cell(move |path, line, _, _| {
                    calls.borrow_mut().push((path.clone(), line));
                    (line == 10).then(|| div().child("editing").into_any_element())
                }),
        )
    }
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
}

#[gpui_kit::test]
fn dragging_the_divider_resizes_columns_and_reports_the_ratio(cx: &mut TestAppContext) {
    cx.update(super::test_init);
    let state = cx.new(|cx| DiffState::new([two_hunks()], cx).with_mode(DiffMode::Split));
    let calls = Rc::new(RefCell::new(Vec::new()));
    let (_, cx) = cx.add_window_view({
        let (state, calls) = (state.clone(), calls.clone());
        move |_, _| Host { state, calls }
    });
    let events = Rc::new(RefCell::new(Vec::new()));
    let _subscription = cx.update(|_, cx| {
        let events = events.clone();
        cx.subscribe(&state, move |_, event: &DiffEvent, _| {
            events.borrow_mut().push(event.clone())
        })
    });
    for _ in 0..3 {
        draw(cx);
    }
    // The hook sees modified lines only, by one-based line number.
    assert!(
        calls
            .borrow()
            .iter()
            .any(|(path, line)| path == "split.txt" && *line == 10)
    );
    assert!(calls.borrow().iter().all(|(_, line)| *line > 0));

    // Code rows start below the header and its side headings; drag from the middle.
    let y = px(150.);
    let left = Modifiers::default();
    cx.simulate_event(MouseDownEvent {
        button: MouseButton::Left,
        position: point(px(200.), y),
        modifiers: left,
        click_count: 1,
        first_mouse: false,
    });
    // Below the body: the drag follows the pointer anywhere.
    cx.simulate_event(MouseMoveEvent {
        position: point(px(280.), px(350.)),
        pressed_button: Some(MouseButton::Left),
        modifiers: left,
    });
    cx.simulate_event(MouseUpEvent {
        button: MouseButton::Left,
        position: point(px(280.), y),
        modifiers: left,
        click_count: 1,
    });
    draw(cx);
    let ratio = cx.update(|_, cx| state.read(cx).split_ratio());
    assert!(ratio > 0.65 && ratio < 0.75, "ratio {ratio}");
    assert!(matches!(
        events.borrow().as_slice(),
        [DiffEvent::SplitRatioChanged(reported)] if *reported == ratio
    ));
}

/// A host whose width the test sets, with a note under modified line 10.
struct Sized {
    state: Entity<DiffState>,
    width: Rc<std::cell::Cell<gpui_kit::Pixels>>,
}

impl Render for Sized {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let note = super::DiffAnnotation::line(
            "note",
            DiffLinePosition::new("split.txt", DiffSide::Modified, 10),
        );
        // 340, not the vendored test's 300: with this platform's 21 px rows the note ended below the view.
        div().w(self.width.get()).h(px(340.)).child(
            Diff::new(&self.state)
                .size_full()
                .annotations([note])
                .on_add_annotation(|_, _, _| {})
                .annotation_hints("to the right", "to the left")
                .render_annotation(|_, _, _| {
                    use gpui_kit::InteractiveElement as _;
                    use gpui_kit::base::TestSupportExt as _;
                    div().id("note-body").test_support().h(px(20.))
                }),
        )
    }
}

fn sized(
    cx: &mut TestAppContext,
    file: super::DiffFile,
) -> (
    Entity<DiffState>,
    Rc<std::cell::Cell<gpui_kit::Pixels>>,
    &mut VisualTestContext,
) {
    cx.update(super::test_init);
    let state = cx.new(|cx| DiffState::new([file], cx).with_mode(DiffMode::Split));
    let width = Rc::new(std::cell::Cell::new(px(400.)));
    let (_, cx) = cx.add_window_view({
        let (state, width) = (state.clone(), width.clone());
        move |_, _| Sized { state, width }
    });
    for _ in 0..3 {
        draw(cx);
    }
    (state, width, cx)
}

fn find(
    cx: &mut VisualTestContext,
    id: impl Into<gpui_kit::ElementId>,
) -> Vec<gpui_kit::Bounds<gpui_kit::Pixels>> {
    let id = id.into();
    cx.update(|window, _| {
        gpui_kit::base::test_support::find_all(window, &[], &id)
            .iter()
            .filter(|e| e.visible())
            .map(|e| e.bounds())
            .collect()
    })
}

#[gpui_kit::test]
fn a_resize_found_while_painting_lays_out_again(cx: &mut TestAppContext) {
    let (state, width, cx) = sized(cx, two_hunks());
    let notified = Rc::new(std::cell::Cell::new(0));
    let _observe = cx.update(|_, cx| {
        let notified = notified.clone();
        cx.observe(&state, move |_, _| notified.set(notified.get() + 1))
    });
    width.set(px(600.));
    cx.update(|window, _| window.refresh());
    draw(cx);
    // The paint saw the new width; the row layout must follow on a next frame of its own.
    assert!(notified.get() > 0, "no frame was asked for");
    assert_eq!(cx.update(|_, cx| state.read(cx).viewport().width), px(600.));
}

#[gpui_kit::test]
fn only_code_scrolls_sideways_and_the_narrow_column_reaches_its_longest_line(
    cx: &mut TestAppContext,
) {
    // The long line is on the original side, in the narrower column.
    let original = format!("{}\n", "x".repeat(400)) + &lines("line", 20);
    let modified = lines("line", 20).replace("line 9\n", "changed 10\n");
    let (state, _, cx) = sized(cx, fixture::modified("split.txt", &original, &modified));
    cx.update(|_, cx| state.update(cx, |state, cx| state.set_split_ratio(0.3, cx)));
    draw(cx);
    let frame = |cx: &mut VisualTestContext| {
        let divider = find(cx, "split-divider")[0].left();
        let gutter = find(cx, ("line", 9usize));
        let note = find(cx, "note-body");
        assert_eq!(note.len(), 1);
        (
            divider,
            gutter.iter().map(|b| b.left()).collect::<Vec<_>>(),
            note[0].left(),
        )
    };
    let before = frame(cx);
    // Every line's code stays inside the 400 px view.
    assert!(find(cx, "source").iter().all(|s| s.right() <= px(400.)));
    let long_end = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            let geometry = state.read(cx).geometry().borrow().run_bounds();
            (geometry.iter())
                .filter(|(side, ix, _)| *side == DiffSide::Original && *ix == 0)
                .map(|(_, _, b)| b.right())
                .fold(px(0.), gpui_kit::Pixels::max)
        })
    };
    assert!(
        long_end(cx) > before.0,
        "the long line runs past its column"
    );
    // Selection hit tests see only the part in its column.
    let visible = cx.update(|_, cx| state.read(cx).geometry().borrow().visible_bounds());
    assert!(
        (visible.iter())
            .filter(|(side, _)| *side == DiffSide::Original)
            .all(|(_, b)| b.right() <= before.0 + px(3.))
    );
    // As far right as it goes: the long line's end is in its column; the frame didn't move.
    cx.update(|_, cx| {
        state.update(cx, |state, _| {
            state
                .horizontal_scroll()
                .set_offset(point(px(-10000.), px(0.)))
        })
    });
    for _ in 0..3 {
        draw(cx);
    }
    let end = long_end(cx);
    assert!(
        end <= before.0 && end > before.0 - px(60.),
        "end {end:?}, divider {:?}",
        before.0
    );
    assert_eq!(frame(cx), before);
}

#[gpui_kit::test]
fn the_other_column_hints_at_an_annotation_beside_its_gap(cx: &mut TestAppContext) {
    let (_, _, cx) = sized(cx, two_hunks());
    let (hint, note, divider) = (
        find(cx, "annotation-hint"),
        find(cx, "note-body"),
        find(cx, "split-divider")[0],
    );
    assert_eq!(hint.len(), 1, "only beside the note");
    assert!(
        hint[0].right() <= divider.center().x,
        "in the original column"
    );
    let middle = hint[0].center().y;
    assert!(
        note[0].top() <= middle && middle <= note[0].bottom(),
        "{hint:?} {note:?}"
    );
}

#[gpui_kit::test]
fn only_the_hovered_line_has_an_add_button(cx: &mut TestAppContext) {
    let (state, _, cx) = sized(cx, two_hunks());
    let rows = |cx: &mut VisualTestContext| {
        (0..60usize)
            .flat_map(|ix| find(cx, ("add-annotation", ix)))
            .collect::<Vec<_>>()
    };
    assert!(rows(cx).is_empty());
    let line = |cx: &mut VisualTestContext, n: usize| {
        let at = DiffLinePosition::new("split.txt", DiffSide::Modified, n);
        cx.update(|_, cx| {
            state.update(cx, |state, cx| {
                state.hovered = Some(at);
                cx.notify()
            })
        });
        draw(cx);
        rows(cx)
    };
    assert_eq!(line(cx, 10).len(), 1);
    let buttons = line(cx, 11);
    assert_eq!(buttons.len(), 1, "the last line's button went");
    // The button sits over the change sign, a rem wide, clear of the code.
    assert!(buttons[0].size.width <= px(16.), "{buttons:?}");
}

#[gpui_kit::test]
fn showing_a_line_in_view_keeps_the_scroll(cx: &mut TestAppContext) {
    let (state, _, cx) = sized(cx, two_hunks());
    let top = ListOffset {
        item_ix: 1,
        offset_in_item: px(15.),
    };
    cx.update(|_, cx| state.update(cx, |state, _| state.list().scroll_to(top)));
    draw(cx);
    cx.update(|_, cx| {
        state.update(cx, |state, cx| {
            // The last line fully in view.
            let viewport = state.list().viewport_bounds();
            let line = (1..=60)
                .rev()
                .map(|line| DiffLinePosition::new("split.txt", DiffSide::Modified, line))
                .find(|line| {
                    (state.source_row(line))
                        .and_then(|row| state.list().bounds_for_item(row))
                        .is_some_and(|b| {
                            b.top() >= viewport.top() && b.bottom() <= viewport.bottom()
                        })
                })
                .unwrap();
            state.show_line(line, cx);
            let scrolled = state.list().logical_scroll_top();
            assert_eq!((scrolled.item_ix, scrolled.offset_in_item), (1, px(15.)));
        })
    });
}
