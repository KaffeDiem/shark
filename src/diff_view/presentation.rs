//! Presentation prepared on a background thread: theme-independent syntax
//! captures and inline changes between paired lines.
use std::{collections::HashMap, ops::Range};

use gpui_kit::base::input::HighlightStyleResolver;
use gpui_kit::{HighlightStyle, SharedString};
use similar::{ChangeTag, TextDiff};

use super::syntax::{SyntaxHighlighter, language_token};
use super::{DiffFile, DiffSide, document::SourceLine};

/// Paired lines sharing less than this proportion of content are emphasized as
/// whole lines only; marking nearly every character adds noise, not signal.
const MIN_INLINE_SIMILARITY: f32 = 0.25;

/// Settings that determine prepared presentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PresentationOptions {
    pub inline_max_line_length: usize,
    pub syntax_max_line_length: usize,
}

/// Highlighters by language, shared by the files of one preparation pass
/// because building one compiles its language's queries.
pub(crate) type SyntaxHighlighters = HashMap<SharedString, SyntaxHighlighter>;

/// Prepared presentation for one file.
#[derive(Default)]
pub(crate) struct FilePresentation {
    names: Vec<SharedString>,
    syntax: [LineSpans<u16>; 2],
    inline: [LineSpans<()>; 2],
}

/// Byte ranges relative to each line's source start, with a payload.
struct LineSpans<T> {
    /// `starts[ix]..starts[ix + 1]` index the spans of line `ix`.
    starts: Vec<u32>,
    spans: Vec<(Range<u32>, T)>,
}

impl<T> Default for LineSpans<T> {
    fn default() -> Self {
        Self {
            starts: Vec::new(),
            spans: Vec::new(),
        }
    }
}

impl<T> LineSpans<T> {
    fn from_lines(per_line: Vec<Vec<(Range<u32>, T)>>) -> Self {
        let mut spans = Self::default();
        spans.starts.reserve(per_line.len() + 1);
        spans.starts.push(0);
        for line in per_line {
            spans.spans.extend(line);
            spans.starts.push(spans.spans.len() as u32);
        }
        spans
    }

    fn line(&self, ix: usize) -> &[(Range<u32>, T)] {
        match (self.starts.get(ix), self.starts.get(ix + 1)) {
            (Some(start), Some(end)) => &self.spans[*start as usize..*end as usize],
            _ => &[],
        }
    }
}

impl FilePresentation {
    /// Blocking; run it on a background thread.
    pub(crate) fn prepare(
        file: &DiffFile,
        options: PresentationOptions,
        highlighters: &mut SyntaxHighlighters,
    ) -> Self {
        let mut presentation = Self::default();
        let mut name_ixs = HashMap::<SharedString, u16>::new();
        for side in [DiffSide::Original, DiffSide::Modified] {
            let per_line = syntax_spans(
                file,
                side,
                options.syntax_max_line_length,
                highlighters,
                &mut |name| {
                    let next = name_ixs.len().min(u16::MAX as usize) as u16;
                    *name_ixs.entry(name.clone()).or_insert_with(|| {
                        presentation.names.push(name.clone());
                        next
                    })
                },
            );
            presentation.syntax[side as usize] = LineSpans::from_lines(per_line);
        }
        presentation.inline = inline_changes(file, options.inline_max_line_length);
        presentation
    }

    /// Theme styles for one line's syntax, as display ranges.
    pub(crate) fn syntax_highlights(
        &self,
        line: &SourceLine,
        side: DiffSide,
        ix: usize,
        theme: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        self.syntax[side as usize]
            .line(ix)
            .iter()
            .filter_map(|(range, name)| {
                let style = theme.style(&self.names[*name as usize])?;
                Some((
                    line.display_range(range.start as usize..range.end as usize),
                    style,
                ))
            })
            .collect()
    }

    /// Changed runs within one line, as display ranges.
    pub(crate) fn inline_changes<'a>(
        &'a self,
        line: &'a SourceLine,
        side: DiffSide,
        ix: usize,
    ) -> impl Iterator<Item = Range<usize>> + 'a {
        self.inline[side as usize]
            .line(ix)
            .iter()
            .map(|(range, _)| line.display_range(range.start as usize..range.end as usize))
    }
}

/// Parses each hunk of one side independently, so an unterminated comment or
/// string in one hunk cannot affect the next.
fn syntax_spans(
    file: &DiffFile,
    side: DiffSide,
    max_line_length: usize,
    highlighters: &mut SyntaxHighlighters,
    name_ix: &mut dyn FnMut(&SharedString) -> u16,
) -> Vec<Vec<(Range<u32>, u16)>> {
    let lines = file.lines(side);
    let mut per_line: Vec<Vec<(Range<u32>, u16)>> = (0..lines.len()).map(|_| Vec::new()).collect();
    let Some(path) = file.side_path(side) else {
        return per_line;
    };
    if lines.is_empty() {
        return per_line;
    }
    let language = language_token(path);
    let highlighter = highlighters
        .entry(language.clone())
        .or_insert_with(|| SyntaxHighlighter::new(&language));
    let ranges = if file.hunks().is_empty() {
        vec![0..lines.len()]
    } else {
        file.hunks().iter().map(|hunk| hunk.lines(side)).collect()
    };
    let source = file.source(side);
    for range in ranges.into_iter().filter(|range| !range.is_empty()) {
        let base = lines[range.start].source().start;
        let text = &source[base..lines[range.end - 1].source().end];
        let hunk_lines = &lines[range.clone()];
        for (capture, name) in highlighter.capture_names(text) {
            let capture = capture.start + base..capture.end + base;
            let first = hunk_lines.partition_point(|line| line.source().end <= capture.start);
            for (offset, line) in hunk_lines[first..].iter().enumerate() {
                let line_start = line.source().start;
                if line_start >= capture.end {
                    break;
                }
                if line.content_end() - line_start > max_line_length {
                    continue;
                }
                let start = capture.start.max(line_start);
                let end = capture.end.min(line.content_end());
                if start < end {
                    per_line[range.start + first + offset].push((
                        (start - line_start) as u32..(end - line_start) as u32,
                        name_ix(&name),
                    ));
                }
            }
        }
    }
    per_line
}

/// Compares each deleted line with the added line paired with it.
fn inline_changes(file: &DiffFile, max_line_length: usize) -> [LineSpans<()>; 2] {
    let mut sides: [Vec<Vec<(Range<u32>, ())>>; 2] = [
        (0..file.lines_count(DiffSide::Original))
            .map(|_| Vec::new())
            .collect(),
        (0..file.lines_count(DiffSide::Modified))
            .map(|_| Vec::new())
            .collect(),
    ];
    for pair in file.pairs() {
        let (true, Some(old_ix), Some(new_ix)) =
            (pair.is_changed(), pair.original(), pair.modified())
        else {
            continue;
        };
        let old = file.lines(DiffSide::Original)[old_ix].text();
        let new = file.lines(DiffSide::Modified)[new_ix].text();
        if old.len() > max_line_length || new.len() > max_line_length || old == new {
            continue;
        }
        let (old_runs, new_runs) = changed_runs(old, new);
        sides[0][old_ix] = old_runs.into_iter().map(|range| (range, ())).collect();
        sides[1][new_ix] = new_runs.into_iter().map(|range| (range, ())).collect();
    }
    let [original, modified] = sides;
    [
        LineSpans::from_lines(original),
        LineSpans::from_lines(modified),
    ]
}

/// Byte ranges that differ between two lines, on each side.
pub(crate) fn changed_runs(old: &str, new: &str) -> (Vec<Range<u32>>, Vec<Range<u32>>) {
    let diff = TextDiff::from_unicode_words(old, new);
    if diff.ratio() < MIN_INLINE_SIMILARITY {
        return (Vec::new(), Vec::new());
    }
    let (mut old_runs, mut new_runs) = (Vec::new(), Vec::new());
    let (mut old_offset, mut new_offset) = (0usize, 0usize);
    for change in diff.iter_all_changes() {
        let len = change.value().len();
        match change.tag() {
            ChangeTag::Equal => {
                old_offset += len;
                new_offset += len;
            }
            ChangeTag::Delete => {
                push_run(&mut old_runs, old_offset..old_offset + len, old);
                old_offset += len;
            }
            ChangeTag::Insert => {
                push_run(&mut new_runs, new_offset..new_offset + len, new);
                new_offset += len;
            }
        }
    }
    (old_runs, new_runs)
}

/// Extends the previous run when only whitespace separates it from `range`.
fn push_run(runs: &mut Vec<Range<u32>>, range: Range<usize>, text: &str) {
    if let Some(last) = runs.last_mut() {
        let gap = &text[last.end as usize..range.start];
        if gap.trim().is_empty() {
            last.end = range.end as u32;
            return;
        }
    }
    runs.push(range.start as u32..range.end as u32);
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPTIONS: PresentationOptions = PresentationOptions {
        inline_max_line_length: 1000,
        syntax_max_line_length: 1000,
    };

    #[test]
    fn inline_changes_merge_words_across_whitespace_and_skip_unrelated_lines() {
        let (old, new) = changed_runs("let total = price * count;", "let total = cost * quantity;");
        assert_eq!(old, [12..17, 20..25]);
        assert_eq!(new, [12..16, 19..27]);
        let (old, new) = changed_runs("old value here", "new value here");
        assert_eq!((old, new), (vec![0..3], vec![0..3]));
        let (old, new) = changed_runs("a b c", "a x y c");
        assert_eq!(new, [2..5]);
        assert!(old.is_empty() || old == [2..3]);
        let (old, new) = changed_runs("alpha", "zzzzzzzzz");
        assert!(old.is_empty() && new.is_empty());
    }

    #[test]
    fn inline_changes_cover_paired_lines_only() {
        let file = DiffFile::parse(
            "--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,3 @@\n-let total = price * count;\n same\n+let total = cost * count;\n+added\n",
        )
        .unwrap()
        .remove(0);
        let presentation =
            FilePresentation::prepare(&file, OPTIONS, &mut SyntaxHighlighters::default());
        let ranges = |side, ix| {
            let line = &file.lines(side)[ix];
            presentation
                .inline_changes(line, side, ix)
                .collect::<Vec<_>>()
        };
        // The deletion and the first addition are in different change runs,
        // so they are not paired.
        assert!(ranges(DiffSide::Original, 0).is_empty());
        let file = DiffFile::parse(
            "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1,2 @@\n-let total = price * count;\n+let total = cost * count;\n+added\n",
        )
        .unwrap()
        .remove(0);
        let presentation =
            FilePresentation::prepare(&file, OPTIONS, &mut SyntaxHighlighters::default());
        let ranges = |side, ix| {
            let line = &file.lines(side)[ix];
            presentation
                .inline_changes(line, side, ix)
                .collect::<Vec<_>>()
        };
        assert_eq!(ranges(DiffSide::Original, 0), [12..17]);
        assert_eq!(ranges(DiffSide::Modified, 0), [12..16]);
        assert!(ranges(DiffSide::Modified, 1).is_empty());
    }

    #[test]
    fn syntax_is_parsed_per_hunk_and_respects_line_limit() {
        // Hunks are separated by unavailable source. Parsing their fragments
        // together would join `/* open` and `close */` into one comment.
        let file = DiffFile::parse(
            "--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n-let a = 1;\n+let a = 2;\n /* open\n@@ -10,2 +10,2 @@\n close */\n-fn old() {}\n+fn new() {}\n",
        )
        .unwrap()
        .remove(0);
        let presentation =
            FilePresentation::prepare(&file, OPTIONS, &mut SyntaxHighlighters::default());
        let names = |presentation: &FilePresentation, ix: usize| {
            presentation.syntax[DiffSide::Modified as usize]
                .line(ix)
                .iter()
                .map(|(_, name)| presentation.names[*name as usize].to_string())
                .collect::<Vec<_>>()
        };
        assert!(names(&presentation, 0).iter().any(|name| name == "keyword"));
        assert!(
            !names(&presentation, 2)
                .iter()
                .any(|name| name.starts_with("comment"))
        );
        assert!(
            names(&presentation, 3)
                .iter()
                .any(|name| name == "function")
        );

        let short = PresentationOptions {
            syntax_max_line_length: 5,
            ..OPTIONS
        };
        let presentation =
            FilePresentation::prepare(&file, short, &mut SyntaxHighlighters::default());
        assert!(names(&presentation, 0).is_empty());
    }
}
