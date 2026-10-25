use std::{cell::RefCell, rc::Rc};

use gpui::{
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

#[gpui::test]
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

#[gpui::test]
fn replace_file_keeps_scroll_and_shows_new_lines(cx: &mut TestAppContext) {
    let state = cx.new(|cx| {
        DiffState::new([two_hunks(), fixture::modified("b.txt", "b\n", "B\n")], cx)
            .with_mode(DiffMode::Split)
    });
    state.update(cx, |state, cx| {
        state.set_file_collapsed("b.txt", true, cx);
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
        assert!(state.is_file_collapsed("b.txt"));
        assert_eq!(state.files()[0].additions(), 3);
        // Unknown paths are ignored.
        state.replace_file(fixture::modified("missing.txt", "a\n", "b\n"), cx);
        assert_eq!(state.files().len(), 2);

        // Line 30 sits in the fold between the hunks until it is shown.
        let line_30 = DiffLinePosition::new("split.txt", DiffSide::Modified, 31);
        let shown = |state: &DiffState| {
            state
                .source_row(&line_30)
                .is_some_and(|row| matches!(state.rows()[row], DisplayRow::Code { .. }))
        };
        assert!(!shown(state));
        state.show_line(line_30.clone(), cx);
        assert!(shown(state));
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

#[gpui::test]
fn dragging_the_divider_resizes_columns_and_reports_the_ratio(cx: &mut TestAppContext) {
    cx.update(crate::init);
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
    cx.simulate_event(MouseMoveEvent {
        position: point(px(280.), y),
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
