use gpui_kit::{AppContext, Empty, ListOffset, TestAppContext, px, size};

use super::{
    DiffFile, DiffLinePosition, DiffLineRange, DiffMode, DiffSide, DiffState, rows::DisplayRow,
    state::FoldExpansion,
};

const PATCH: &str = "diff --git a/first.txt b/first.txt\n--- a/first.txt\n+++ b/first.txt\n@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\ndiff --git a/second.txt b/second.txt\n--- a/second.txt\n+++ b/second.txt\n@@ -10 +10 @@\n-ten\n+TEN\n";

fn code_rows(state: &DiffState, file: usize) -> usize {
    state
        .rows()
        .iter()
        .filter(|row| matches!(row, DisplayRow::Code { file: f, .. } if *f == file))
        .count()
}

#[gpui_kit::test]
fn collapsed_files_keep_their_header_and_survive_replacement(cx: &mut TestAppContext) {
    let state = cx.new(|cx| DiffState::new(DiffFile::parse(PATCH).unwrap(), cx));
    state.update(cx, |state, cx| {
        assert!(code_rows(state, 0) > 0);
        state.set_file_collapsed("first.txt", true, cx);
        assert!(state.is_file_collapsed("first.txt"));
        assert_eq!(code_rows(state, 0), 0);
        assert!(matches!(state.rows()[0], DisplayRow::File(0)));
        assert!(matches!(state.rows()[1], DisplayRow::File(1)));
        state.toggle_file_collapsed(1, cx);
        assert!(state.is_file_collapsed("second.txt"));

        // Revealing a line expands its file.
        state.scroll_to_line(
            DiffLinePosition::new("first.txt", DiffSide::Modified, 2),
            cx,
        );
        assert!(!state.is_file_collapsed("first.txt"));

        // Paths identify files, so collapse survives a newer patch revision.
        state.set_files(DiffFile::parse(PATCH).unwrap().into_iter().rev(), cx);
        assert!(state.is_file_collapsed("second.txt"));
        state.scroll_to_file("first.txt", cx);
        assert_eq!(state.list().logical_scroll_top().item_ix, 1);
        state.scroll_to_file("missing.txt", cx);
        assert_eq!(state.list().logical_scroll_top().item_ix, 1);
    });
}

#[gpui_kit::test]
fn folds_expand_by_direction_and_short_ranges_stay_visible(cx: &mut TestAppContext) {
    let original = (1..=60)
        .map(|line| format!("line {line}\n"))
        .collect::<String>();
    let modified = original
        .replace("line 30\n", "changed 30\n")
        .replace("line 60\n", "changed 60\n");
    let file = super::document::fixture::modified("long.txt", &original, &modified);
    let state = cx.new(|cx| DiffState::new([file], cx));
    let folds = |state: &DiffState| {
        state
            .rows()
            .iter()
            .filter_map(|row| match row {
                DisplayRow::Fold { pairs, .. } => Some(pairs.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    state.update(cx, |state, cx| {
        state.set_context_lines(Some(2), cx);
        assert_eq!(folds(state), [0..27, 32..57]);
        // A disclosure reveals 20 lines at most.
        state.expand_fold(0, 32..57, FoldExpansion::Down, cx);
        assert_eq!(folds(state), [0..27, 52..57]);
        state.expand_fold(0, 0..27, FoldExpansion::Up, cx);
        assert_eq!(folds(state), [0..7, 52..57]);
        state.expand_fold(0, 0..7, FoldExpansion::All, cx);
        assert_eq!(folds(state), [52..57]);
        state.collapse_unchanged(cx);
        assert_eq!(folds(state), [0..27, 32..57]);
    });
}

#[gpui_kit::test]
fn line_selection_follows_a_gesture(cx: &mut TestAppContext) {
    let window = cx.open_window(size(px(640.), px(480.)), |_, _| Empty);
    let state = cx.new(|cx| DiffState::new(DiffFile::parse(PATCH).unwrap(), cx));
    let position = |side, line| DiffLinePosition::new("first.txt", side, line);
    cx.update_window(window.into(), |_, window, cx| {
        state.update(cx, |state, cx| {
            state.begin_line_selection(position(DiffSide::Original, 1), false, window, cx);
            assert_eq!(
                state.selected_lines(),
                Some(DiffLineRange::new("first.txt", DiffSide::Original, 1, 1))
            );
            state.drag_line_selection(position(DiffSide::Original, 2), cx);
            assert_eq!(
                state.selected_lines(),
                Some(DiffLineRange::new("first.txt", DiffSide::Original, 1, 2))
            );
            // A drag into another file does not extend the selection.
            state.drag_line_selection(
                DiffLinePosition::new("second.txt", DiffSide::Original, 10),
                cx,
            );
            // In Unified, dragging onto the addition crosses sides.
            state.drag_line_selection(position(DiffSide::Modified, 2), cx);
            state.end_gesture(cx);
            // A drag after the gesture ended is ignored.
            state.drag_line_selection(position(DiffSide::Original, 3), cx);
        });
    })
    .unwrap();
    let cross =
        DiffLineRange::new("first.txt", DiffSide::Original, 1, 2).with_end_side(DiffSide::Modified);
    cx.update(|cx| {
        let state = state.read(cx);
        assert_eq!(state.selected_lines(), Some(cross.clone()));
        assert_eq!(state.selected_text(cx), "one\ntwo\nTWO\n");
    });
}

#[gpui_kit::test]
fn programmatic_cross_side_ranges_clip_to_supplied_lines(cx: &mut TestAppContext) {
    let state = cx.new(|cx| DiffState::new(DiffFile::parse(PATCH).unwrap(), cx));
    state.update(cx, |state, cx| {
        state.set_selected_lines(
            Some(
                DiffLineRange::new("first.txt", DiffSide::Original, 2, 99)
                    .with_end_side(DiffSide::Modified),
            ),
            cx,
        );
        assert_eq!(
            state.selected_lines(),
            Some(
                DiffLineRange::new("first.txt", DiffSide::Original, 2, 3)
                    .with_end_side(DiffSide::Modified)
            )
        );
        assert_eq!(state.selected_text(cx), "two\nTWO\nthree\n");
        // A single-side range is normalized.
        state.set_selected_lines(
            Some(DiffLineRange::new("first.txt", DiffSide::Modified, 3, 1)),
            cx,
        );
        assert_eq!(
            state.selected_lines(),
            Some(DiffLineRange::new("first.txt", DiffSide::Modified, 1, 3))
        );
        state.list().scroll_to(ListOffset::default());
    });
}

#[gpui_kit::test]
fn unchanged_files_show_every_line_without_folding(cx: &mut TestAppContext) {
    let text = (1..=40)
        .map(|line| format!("line {line}\n"))
        .collect::<String>();
    let state = cx.new(|cx| {
        DiffState::new([DiffFile::unchanged("notes.txt", &text)], cx).with_mode(DiffMode::Split)
    });
    state.update(cx, |state, cx| {
        assert_eq!(code_rows(state, 0), 40);
        assert!(
            !state
                .rows()
                .iter()
                .any(|row| matches!(row, DisplayRow::Fold { .. }))
        );
        state.set_selected_lines(
            Some(DiffLineRange::new("notes.txt", DiffSide::Modified, 2, 3)),
            cx,
        );
        assert_eq!(state.selected_text(cx), "line 2\nline 3\n");
    });
}
