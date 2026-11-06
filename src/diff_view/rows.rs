//! Projects files into virtualized rows: pure functions of the files, folds and mode.
use std::{collections::HashSet, ops::Range};

use gpui_kit::SharedString;

use super::{
    DiffFile, DiffFileStatus, DiffMode, DiffSide,
    document::{run_end, split_run},
};

/// Unchanged lines kept around each change.
pub(crate) const CONTEXT_LINES: usize = 3;
/// Lines an up or down disclosure reveals.
pub(crate) const EXPANSION_LINES: usize = 20;
/// The shortest unchanged range that collapses; shorter ranges stay visible.
const MIN_COLLAPSED_LINES: usize = 2;

/// One virtualized row. Every row belongs to exactly one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DisplayRow {
    /// The file header, with any file-level annotations.
    File(usize),
    /// Binary, metadata-only or empty file summary, for a file without source rows.
    Notice(usize),
    Hunk {
        file: usize,
        hunk: usize,
    },
    Fold {
        file: usize,
        pairs: Range<usize>,
    },
    Code {
        file: usize,
        original: Option<usize>,
        modified: Option<usize>,
        changed: bool,
    },
}

impl DisplayRow {
    pub(crate) fn file(&self) -> usize {
        match self {
            Self::File(file) | Self::Notice(file) => *file,
            Self::Hunk { file, .. } | Self::Fold { file, .. } | Self::Code { file, .. } => *file,
        }
    }
}

/// The row showing each source line, or the disclosure hiding it.
#[derive(Default)]
pub(crate) struct FileRows {
    pub(crate) original: Vec<Option<usize>>,
    pub(crate) modified: Vec<Option<usize>>,
}

/// The rows of every file: a header, then its notice, or its hunks, folds and code.
pub(crate) fn project_rows(
    files: &[DiffFile],
    collapsed: &HashSet<SharedString>,
    mode: DiffMode,
    context: Option<usize>,
    expanded: &[Vec<Range<usize>>],
) -> Vec<DisplayRow> {
    let mut rows = Vec::new();
    for (ix, file) in files.iter().enumerate() {
        rows.push(DisplayRow::File(ix));
        if collapsed.contains(file.path()) {
            continue;
        }
        if file.pairs().is_empty() {
            rows.push(DisplayRow::Notice(ix));
        } else if file.status() == DiffFileStatus::Unchanged {
            rows.extend((0..file.pairs().len()).map(|line| DisplayRow::Code {
                file: ix,
                original: None,
                modified: Some(line),
                changed: false,
            }));
        } else {
            project_file(&mut rows, ix, file, mode, context, &expanded[ix]);
        }
    }
    rows
}

/// Every source line maps to its code row or the disclosure hiding it; lines
/// of a collapsed file have no row. Disjoint folds visit each pair once, so
/// building the maps is linear.
pub(crate) fn source_rows(files: &[DiffFile], rows: &[DisplayRow]) -> Vec<FileRows> {
    let mut file_rows = files
        .iter()
        .map(|file| FileRows {
            original: vec![None; file.lines_count(DiffSide::Original)],
            modified: vec![None; file.lines_count(DiffSide::Modified)],
        })
        .collect::<Vec<_>>();
    for (row_ix, row) in rows.iter().enumerate() {
        let mut map = |file: usize, original: Option<usize>, modified: Option<usize>| {
            if let Some(line) = original {
                file_rows[file].original[line] = Some(row_ix);
            }
            if let Some(line) = modified {
                file_rows[file].modified[line] = Some(row_ix);
            }
        };
        match row {
            DisplayRow::Code {
                file,
                original,
                modified,
                ..
            } => map(*file, *original, *modified),
            DisplayRow::Fold { file, pairs } => {
                for pair in &files[*file].pairs()[pairs.clone()] {
                    map(*file, pair.original(), pair.modified());
                }
            }
            DisplayRow::File(_) | DisplayRow::Notice(_) | DisplayRow::Hunk { .. } => {}
        }
    }
    file_rows
}

/// The first row of each run of changed code rows.
pub(crate) fn changed_groups(rows: &[DisplayRow]) -> Vec<usize> {
    let changed = |row: &DisplayRow| matches!(row, DisplayRow::Code { changed: true, .. });
    rows.iter()
        .enumerate()
        .filter_map(|(ix, row)| {
            (changed(row)
                && (ix == 0 || !changed(&rows[ix - 1]) || rows[ix - 1].file() != row.file()))
            .then_some(ix)
        })
        .collect()
}

/// The end of the hunk containing each pair; a fixture without hunks is one.
fn hunk_ends(document: &DiffFile) -> Vec<usize> {
    let pairs = document.pairs();
    let mut hunk_end = vec![pairs.len(); pairs.len()];
    for hunk in document.hunks() {
        hunk_end[hunk.pairs()].fill(hunk.pairs().end);
    }
    hunk_end
}

/// Which pairs show as code: changes with their context, expanded ranges,
/// and collapsed ranges shorter than [`MIN_COLLAPSED_LINES`].
fn visible_pairs(
    document: &DiffFile,
    context: Option<usize>,
    expanded: &[Range<usize>],
) -> Vec<bool> {
    let pairs = document.pairs();
    let hunk_end = hunk_ends(document);
    let mut hunk_start = vec![0; pairs.len()];
    for hunk in document.hunks() {
        hunk_start[hunk.pairs()].fill(hunk.pairs().start);
    }
    // Interval boundaries avoid repeatedly filling overlapping context windows.
    // Projection remains linear in source lines plus the number of disclosures.
    let mut boundaries = vec![0isize; pairs.len() + 1];
    if let Some(context) = context {
        let mut ix = 0;
        while ix < pairs.len() {
            if !pairs[ix].is_changed() {
                ix += 1;
                continue;
            }
            let start = ix;
            ix = run_end(ix, hunk_end[start], |ix| pairs[ix].is_changed());
            boundaries[start.saturating_sub(context).max(hunk_start[start])] += 1;
            boundaries[ix.saturating_add(context).min(hunk_end[start])] -= 1;
        }
    } else {
        boundaries[0] += 1;
        boundaries[pairs.len()] -= 1;
    }
    for range in expanded {
        let start = range.start.min(pairs.len());
        let end = range.end.min(pairs.len());
        if start < end {
            boundaries[start] += 1;
            boundaries[end] -= 1;
        }
    }
    let mut active = 0;
    let mut visible = boundaries[..pairs.len()]
        .iter()
        .map(|boundary| {
            active += boundary;
            active > 0
        })
        .collect::<Vec<_>>();
    // A disclosure shorter than the threshold would hide almost nothing.
    let mut ix = 0;
    while ix < pairs.len() {
        if visible[ix] {
            ix += 1;
            continue;
        }
        let start = ix;
        ix = run_end(ix, hunk_end[start], |ix| !visible[ix]);
        if ix - start < MIN_COLLAPSED_LINES {
            visible[start..ix].fill(true);
        }
    }
    visible
}

fn project_file(
    rows: &mut Vec<DisplayRow>,
    file: usize,
    document: &DiffFile,
    mode: DiffMode,
    context: Option<usize>,
    expanded: &[Range<usize>],
) {
    let pairs = document.pairs();
    let hunks = document.hunks();
    let hunk_end = hunk_ends(document);
    let visible = visible_pairs(document, context, expanded);
    let mut hunk_ix = 0;
    let mut ix = 0;
    while ix < pairs.len() {
        while let Some(hunk) = hunks.get(hunk_ix) {
            if hunk.pairs().start != ix {
                break;
            }
            rows.push(DisplayRow::Hunk {
                file,
                hunk: hunk_ix,
            });
            hunk_ix += 1;
        }
        if !visible[ix] {
            let start = ix;
            ix = run_end(ix, hunk_end[start], |ix| !visible[ix]);
            rows.push(DisplayRow::Fold {
                file,
                pairs: start..ix,
            });
        } else if mode == DiffMode::Unified && pairs[ix].is_changed() {
            let start = ix;
            ix = run_end(ix, hunk_end[start], |ix| pairs[ix].is_changed());
            rows.extend(split_run(&pairs[start..ix]).map(|(original, modified)| {
                DisplayRow::Code {
                    file,
                    original,
                    modified,
                    changed: true,
                }
            }));
        } else {
            let pair = pairs[ix];
            rows.push(DisplayRow::Code {
                file,
                original: pair.original(),
                modified: pair.modified(),
                changed: pair.is_changed(),
            });
            ix += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_view::document::fixture;

    fn code(rows: &[DisplayRow]) -> Vec<(Option<usize>, Option<usize>)> {
        rows.iter()
            .filter_map(|row| match row {
                DisplayRow::Code {
                    original, modified, ..
                } => Some((*original, *modified)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn unified_rows_list_deletions_before_additions_and_fold_far_context() {
        let old = (0..20).map(|n| format!("l{n}\n")).collect::<String>();
        let new = old.replace("l10\n", "x\ny\n").replace("l11\n", "");
        let file = fixture::modified("a.txt", &old, &new);
        let rows = project_rows(
            std::slice::from_ref(&file),
            &HashSet::new(),
            DiffMode::Unified,
            Some(CONTEXT_LINES),
            &[Vec::new()],
        );
        assert_eq!(rows[0], DisplayRow::File(0));
        // Three lines of context each side of the change; the rest folds.
        assert!(matches!(rows[1], DisplayRow::Fold { ref pairs, .. } if *pairs == (0..7)));
        let changed: Vec<_> = rows
            .iter()
            .filter(|row| matches!(row, DisplayRow::Code { changed: true, .. }))
            .collect();
        // Two deletions, then two additions.
        assert_eq!(changed.len(), 4);
        assert!(matches!(
            changed[0],
            DisplayRow::Code {
                original: Some(_),
                modified: None,
                ..
            }
        ));
        assert!(matches!(
            changed[3],
            DisplayRow::Code {
                original: None,
                modified: Some(_),
                ..
            }
        ));
        assert_eq!(changed_groups(&rows).len(), 1);
        let split = project_rows(
            std::slice::from_ref(&file),
            &HashSet::new(),
            DiffMode::Split,
            None,
            &[Vec::new()],
        );
        // No folds without a context limit.
        assert!(
            split
                .iter()
                .all(|row| !matches!(row, DisplayRow::Fold { .. }))
        );
        assert!(code(&split).len() > code(&rows).len());
        let mapped = source_rows(std::slice::from_ref(&file), &rows);
        assert_eq!(mapped[0].original[0], Some(1));
    }
}
