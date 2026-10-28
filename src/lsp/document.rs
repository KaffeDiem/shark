//! Coordinates and edits are always interpreted against the snapshot that produced them.
use anyhow::{Result, bail, ensure};
use lsp_types::{
    DocumentChangeOperation, DocumentChanges, OneOf, Position, PositionEncodingKind, Range,
    TextDocumentContentChangeEvent, TextEdit, Uri, WorkspaceEdit,
};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::SystemTime;

static EDIT_EPOCH: AtomicU64 = AtomicU64::new(0);

pub fn document_changed() {
    EDIT_EPOCH.fetch_add(1, Ordering::AcqRel);
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct EditGuard {
    pub epoch: u64,
    pub started: SystemTime,
}

impl EditGuard {
    pub fn capture() -> Self {
        Self {
            epoch: EDIT_EPOCH.load(Ordering::Acquire),
            started: SystemTime::now(),
        }
    }
    pub fn is_current(&self) -> bool {
        self.epoch == EDIT_EPOCH.load(Ordering::Acquire)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub text: Arc<str>,
    pub version: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16,
    Utf32,
    /// GPUI positions: UTF-32 columns on lines ended only by '\n', as its rope counts them.
    Editor,
}

impl From<PositionEncodingKind> for Encoding {
    fn from(value: PositionEncodingKind) -> Self {
        match value.as_str() {
            "utf-8" => Self::Utf8,
            "utf-32" => Self::Utf32,
            _ => Self::Utf16,
        }
    }
}

fn width(c: char, encoding: Encoding) -> usize {
    match encoding {
        Encoding::Utf8 => c.len_utf8(),
        Encoding::Utf16 => c.len_utf16(),
        Encoding::Utf32 | Encoding::Editor => 1,
    }
}

/// Line starts of one text, built once so each conversion is a binary search plus one line scan.
pub struct Lines<'a> {
    text: &'a str,
    /// After each '\n'.
    lf: Vec<usize>,
    /// LSP also ends lines at a bare '\r'; `None` when the text has none.
    lsp: Option<Vec<usize>>,
}

impl<'a> Lines<'a> {
    pub fn new(text: &'a str) -> Self {
        fn starts(breaks: impl Iterator<Item = usize>) -> Vec<usize> {
            std::iter::once(0).chain(breaks.map(|i| i + 1)).collect()
        }
        let bytes = text.as_bytes();
        let bare_cr = |i: usize| bytes[i] == b'\r' && bytes.get(i + 1) != Some(&b'\n');
        let lsp = text.match_indices('\r').any(|(i, _)| bare_cr(i)).then(|| {
            starts(
                text.match_indices(['\r', '\n'])
                    .map(|(i, _)| i)
                    .filter(|&i| bytes[i] == b'\n' || bare_cr(i)),
            )
        });
        Self {
            text,
            lf: starts(text.match_indices('\n').map(|(i, _)| i)),
            lsp,
        }
    }

    fn starts(&self, encoding: Encoding) -> &[usize] {
        match &self.lsp {
            Some(lsp) if encoding != Encoding::Editor => lsp,
            _ => &self.lf,
        }
    }

    /// GPUI offsets are bytes; its position columns count Unicode scalars (UTF-32).
    pub fn position(&self, byte: usize, encoding: Encoding) -> Result<Position> {
        let text = self.text;
        ensure!(
            byte <= text.len() && text.is_char_boundary(byte),
            "invalid byte offset"
        );
        ensure!(
            !(byte > 0
                && text.as_bytes().get(byte - 1) == Some(&b'\r')
                && text.as_bytes().get(byte) == Some(&b'\n')),
            "position splits CRLF"
        );
        let starts = self.starts(encoding);
        let line = starts.partition_point(|&s| s <= byte) - 1;
        Ok(Position::new(
            line.try_into()?,
            text[starts[line]..byte]
                .chars()
                .map(|c| width(c, encoding))
                .sum::<usize>()
                .try_into()?,
        ))
    }

    /// LSP 3.17 clamps a column past the line end to the line end; lines past the end clamp too.
    pub fn offset(&self, pos: Position, encoding: Encoding) -> Result<usize> {
        let starts = self.starts(encoding);
        let Some(&start) = starts.get(pos.line as usize) else {
            return Ok(self.text.len());
        };
        let line = match starts.get(pos.line as usize + 1) {
            Some(&next) => {
                let line = &self.text[start..next];
                let line = line.strip_suffix('\n').unwrap_or(line);
                line.strip_suffix('\r').unwrap_or(line)
            }
            None => &self.text[start..],
        };
        let mut units = 0usize;
        for (byte, c) in line.char_indices() {
            if units == pos.character as usize {
                return Ok(start + byte);
            }
            units += width(c, encoding);
            ensure!(
                units <= pos.character as usize,
                "position splits a Unicode character"
            );
        }
        Ok(start + line.len())
    }

    pub fn convert_range(&self, range: Range, from: Encoding, to: Encoding) -> Result<Range> {
        let start = self.offset(range.start, from)?;
        let end = self.offset(range.end, from)?;
        ensure!(start <= end, "reversed edit range");
        Ok(Range::new(
            self.position(start, to)?,
            self.position(end, to)?,
        ))
    }
}

/// One-off conversions; use [`Lines`] for more than one position in the same text.
pub fn position(text: &str, byte: usize, encoding: Encoding) -> Result<Position> {
    Lines::new(text).position(byte, encoding)
}

pub fn offset(text: &str, pos: Position, encoding: Encoding) -> Result<usize> {
    Lines::new(text).offset(pos, encoding)
}

pub fn convert_range(text: &str, range: Range, from: Encoding, to: Encoding) -> Result<Range> {
    Lines::new(text).convert_range(range, from, to)
}

/// One minimal contiguous change, computed by the host's background executor.
pub fn incremental_change(
    old: &str,
    new: &str,
    encoding: Encoding,
) -> Result<Option<TextDocumentContentChangeEvent>> {
    if old == new {
        return Ok(None);
    }
    let mut start = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(start) || !new.is_char_boundary(start) {
        start -= 1;
    }
    if start > 0
        && (old.as_bytes().get(start - 1) == Some(&b'\r')
            && old.as_bytes().get(start) == Some(&b'\n')
            || new.as_bytes().get(start - 1) == Some(&b'\r')
                && new.as_bytes().get(start) == Some(&b'\n'))
    {
        start -= 1;
    }
    let suffix = old[start..]
        .bytes()
        .rev()
        .zip(new[start..].bytes().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let (mut old_end, mut new_end) = (old.len() - suffix, new.len() - suffix);
    while !old.is_char_boundary(old_end) || !new.is_char_boundary(new_end) {
        old_end += 1;
        new_end += 1;
    }
    if old_end > 0
        && (old.as_bytes().get(old_end - 1) == Some(&b'\r')
            && old.as_bytes().get(old_end) == Some(&b'\n')
            || new_end > 0
                && new.as_bytes().get(new_end - 1) == Some(&b'\r')
                && new.as_bytes().get(new_end) == Some(&b'\n'))
    {
        old_end += 1;
        new_end += 1;
    }
    let lines = Lines::new(old);
    Ok(Some(TextDocumentContentChangeEvent {
        range: Some(Range::new(
            lines.position(start, encoding)?,
            lines.position(old_end, encoding)?,
        )),
        range_length: None,
        text: new[start..new_end].to_owned(),
    }))
}

pub fn apply_edits(
    snapshot: &Snapshot,
    expected_version: Option<i32>,
    edits: &[TextEdit],
    encoding: Encoding,
) -> Result<String> {
    ensure!(
        expected_version.is_none_or(|v| v == snapshot.version),
        "document version changed"
    );
    let lines = Lines::new(&snapshot.text);
    let mut replacements = edits
        .iter()
        .map(|e| {
            let start = lines.offset(e.range.start, encoding)?;
            let end = lines.offset(e.range.end, encoding)?;
            ensure!(start <= end, "reversed edit range");
            Ok((start, end, &e.new_text))
        })
        .collect::<Result<Vec<_>>>()?;
    replacements.sort_by_key(|(start, end, _)| (*start, *end));
    for pair in replacements.windows(2) {
        ensure!(pair[0].1 <= pair[1].0, "overlapping edits");
    }
    let mut result = snapshot.text.to_string();
    for (start, end, text) in replacements.into_iter().rev() {
        result.replace_range(start..end, text);
    }
    ensure!(
        result.len() as u64 <= crate::editor::MAX_BYTES,
        "edited file exceeds the 4 MiB editor limit"
    );
    Ok(result)
}

pub type DocumentEdits = (Uri, Option<i32>, Vec<TextEdit>);

/// Reject unsupported operations before the host starts changing any buffer.
pub fn workspace_edits(edit: WorkspaceEdit) -> Result<Vec<DocumentEdits>> {
    ensure!(
        edit.change_annotations
            .as_ref()
            .is_none_or(|a| a.is_empty()),
        "annotated edits require confirmation and are unsupported"
    );
    let mut result = vec![];
    // LSP 3.17: documentChanges, when present, are preferred over changes.
    if let Some(changes) = edit.changes.filter(|_| edit.document_changes.is_none()) {
        result.extend(changes.into_iter().map(|(uri, edits)| (uri, None, edits)));
    }
    if let Some(changes) = edit.document_changes {
        let edits = match changes {
            DocumentChanges::Edits(edits) => edits,
            DocumentChanges::Operations(ops) => ops
                .into_iter()
                .map(|op| match op {
                    DocumentChangeOperation::Edit(e) => Ok(e),
                    DocumentChangeOperation::Op(_) => {
                        bail!("file create/delete/rename operations are unsupported")
                    }
                })
                .collect::<Result<Vec<_>>>()?,
        };
        for edit in edits {
            let edits = edit
                .edits
                .into_iter()
                .map(|e| match e {
                    OneOf::Left(e) => Ok(e),
                    OneOf::Right(_) => bail!("annotated edits are unsupported"),
                })
                .collect::<Result<Vec<_>>>()?;
            result.push((edit.text_document.uri, edit.text_document.version, edits));
        }
    }
    let mut seen = std::collections::HashSet::new();
    for (uri, _, _) in &result {
        ensure!(
            uri.scheme().is_some_and(|s| s.as_str() == "file"),
            "only local files can be edited"
        );
        ensure!(
            seen.insert(uri.clone()),
            "multiple edit batches for one document are unsupported"
        );
    }
    Ok(result)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    pub text: String,
    /// Byte ranges in the expanded text, sorted by tab order; $0 is last.
    pub tabstops: Vec<(u32, std::ops::Range<usize>)>,
}

/// Expand LSP tabstops/defaults/choices. Unsupported transforms fail rather than insert syntax.
pub fn snippet(source: &str) -> Result<Snippet> {
    let mut out = Snippet {
        text: String::new(),
        tabstops: vec![],
    };
    let mut values = std::collections::HashMap::<u32, String>::new();
    expand_snippet(source, &mut out, &mut values, 0)?;
    // A bare tabstop may precede the occurrence that supplies its default.
    out.text.clear();
    out.tabstops.clear();
    expand_snippet(source, &mut out, &mut values, 0)?;
    out.tabstops.sort_by_key(|(index, _)| tab_order(*index));
    Ok(out)
}

/// $0 is the final stop.
pub fn tab_order(index: u32) -> u32 {
    if index == 0 { u32::MAX } else { index }
}

/// A snippet field, as byte offsets into the accepted document.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Stop {
    pub index: u32,
    pub start: usize,
    pub end: usize,
}

/// Carried on a prepared completion item so the editor can recognize its acceptance.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CompletionMeta {
    pub version: i32,
    /// The folded edit's span in the accepted document, the fingerprint of that span, and
    /// the accepted document's length.
    pub start: usize,
    pub end: usize,
    pub fingerprint: u64,
    pub len: usize,
    pub cursor: usize,
    pub tabstops: Vec<Stop>,
}

fn expand_snippet(
    source: &str,
    out: &mut Snippet,
    values: &mut std::collections::HashMap<u32, String>,
    depth: usize,
) -> Result<()> {
    ensure!(depth < 32, "snippet nesting too deep");
    let mut chars = source.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        // LSP 3.17: a backslash escapes only `$`, `}` and `\`; otherwise it is text.
        if c == '\\'
            && chars
                .peek()
                .is_some_and(|(_, n)| matches!(n, '$' | '}' | '\\'))
        {
            out.text.push(chars.next().unwrap().1);
            continue;
        }
        if c != '$' {
            out.text.push(c);
            continue;
        }
        let mut ahead = chars.clone();
        let braced = ahead.peek().is_some_and(|(_, c)| *c == '{');
        if braced {
            ahead.next();
        }
        match ahead.peek() {
            Some((_, c)) if c.is_ascii_digit() => chars = ahead,
            Some((_, c)) if *c == '_' || c.is_ascii_alphabetic() => {
                bail!("snippet variables are unsupported")
            }
            // Neither a tabstop nor a variable: a literal `$`.
            _ => {
                out.text.push('$');
                continue;
            }
        }
        let mut digits = String::new();
        while chars.peek().is_some_and(|(_, c)| c.is_ascii_digit()) {
            digits.push(chars.next().unwrap().1);
        }
        let index: u32 = digits.parse()?;
        let start = out.text.len();
        if !braced || chars.peek().is_some_and(|(_, c)| *c == '}') {
            if braced {
                chars.next();
            }
            if let Some(value) = values.get(&index) {
                out.text.push_str(value);
            }
        } else {
            let (_, delimiter) = chars
                .next()
                .ok_or_else(|| anyhow::anyhow!("unterminated snippet"))?;
            ensure!(
                delimiter == ':' || delimiter == '|',
                "snippet transforms are unsupported"
            );
            let body_start = chars.peek().map_or(source.len(), |(i, _)| *i);
            let mut nested = 0;
            let mut escaped = false;
            let mut dollar = false;
            let end = loop {
                let (at, c) = chars
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("unterminated snippet"))?;
                if escaped {
                    escaped = false;
                    dollar = false;
                    continue;
                }
                if c == '\\' {
                    escaped = true;
                    continue;
                }
                // Only a nested `${` needs its own `}`; a bare `{` is text.
                if c == '{' && dollar {
                    nested += 1;
                }
                dollar = c == '$';
                if c == '}' {
                    if nested == 0 {
                        break at;
                    }
                    nested -= 1;
                }
            };
            let body = &source[body_start..end];
            if delimiter == ':' {
                expand_snippet(body, out, values, depth + 1)?;
            } else {
                let body = body
                    .strip_suffix('|')
                    .ok_or_else(|| anyhow::anyhow!("unterminated snippet choice"))?;
                let mut choice = body.chars().peekable();
                while let Some(c) = choice.next() {
                    match c {
                        '\\' if choice
                            .peek()
                            .is_some_and(|n| matches!(n, '$' | '}' | '\\' | ',' | '|')) =>
                        {
                            out.text.extend(choice.next());
                        }
                        ',' => break,
                        c => out.text.push(c),
                    }
                }
            }
            values.insert(index, out.text[start..].to_owned());
        }
        out.tabstops.push((index, start..out.text.len()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(text: &str) -> Snapshot {
        Snapshot {
            text: Arc::from(text),
            version: 4,
        }
    }
    #[test]
    fn unicode_and_crlf_positions_roundtrip() {
        let text = "é😀z\r\n中文\n";
        for encoding in [Encoding::Utf8, Encoding::Utf16, Encoding::Utf32] {
            for (byte, _) in text
                .char_indices()
                .chain(std::iter::once((text.len(), ' ')))
            {
                if text.as_bytes().get(byte) == Some(&b'\n')
                    && byte > 0
                    && text.as_bytes()[byte - 1] == b'\r'
                {
                    continue;
                }
                assert_eq!(
                    offset(text, position(text, byte, encoding).unwrap(), encoding).unwrap(),
                    byte
                );
            }
        }
        assert!(offset(text, Position::new(0, 2), Encoding::Utf16).is_err());
        // LSP 3.17: a column past the line end means the line end.
        assert_eq!(
            offset(text, Position::new(1, 7), Encoding::Utf8).unwrap(),
            15
        );
        assert_eq!(
            offset(text, Position::new(0, 99), Encoding::Utf16).unwrap(),
            7
        );
        assert_eq!(
            offset(text, Position::new(4, 0), Encoding::Utf16).unwrap(),
            text.len()
        );
        let bare_cr = "x\r";
        assert_eq!(
            offset(
                bare_cr,
                position(bare_cr, bare_cr.len(), Encoding::Utf16).unwrap(),
                Encoding::Utf16
            )
            .unwrap(),
            bare_cr.len()
        );
    }
    #[test]
    fn bare_cr_ends_an_lsp_line_but_not_an_editor_line() {
        let text = "x\ry\r\nz";
        assert_eq!(
            position(text, 2, Encoding::Utf16).unwrap(),
            Position::new(1, 0)
        );
        assert_eq!(
            position(text, 5, Encoding::Utf16).unwrap(),
            Position::new(2, 0)
        );
        assert_eq!(
            offset(text, Position::new(0, 5), Encoding::Utf8).unwrap(),
            1
        );
        assert_eq!(
            convert_range(
                text,
                Range::new(Position::new(1, 0), Position::new(1, 1)),
                Encoding::Utf16,
                Encoding::Editor
            )
            .unwrap(),
            Range::new(Position::new(0, 2), Position::new(0, 3))
        );
    }
    #[test]
    fn incremental_updates_reconstruct_document() {
        for (old, new) in [
            ("a😀\r\nb", "a🌟\r\nb"),
            ("a\r\nb", "a\nb"),
            ("", "é"),
            ("😀", ""),
            ("a\r\nb", "a\r\nxb"),
            ("éé", "é"),
            ("x\n", "x\r\n"),
            ("x\r", "x\r\n"),
            ("x\r", "x"),
        ] {
            for encoding in [Encoding::Utf8, Encoding::Utf16, Encoding::Utf32] {
                let change = incremental_change(old, new, encoding).unwrap().unwrap();
                assert_eq!(
                    apply_edits(
                        &snapshot(old),
                        Some(4),
                        &[TextEdit::new(change.range.unwrap(), change.text)],
                        encoding
                    )
                    .unwrap(),
                    new
                );
            }
        }
        assert!(
            incremental_change("x", "x", Encoding::Utf16)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn incremental_updates_reconstruct_short_unicode_and_newline_pairs() {
        let atoms = ["a", "é", "😀", "\r", "\n"];
        let mut texts = vec![String::new()];
        for a in atoms {
            texts.push(a.into());
            for b in atoms {
                texts.push(format!("{a}{b}"));
            }
        }
        for old in &texts {
            for new in &texts {
                for encoding in [Encoding::Utf8, Encoding::Utf16, Encoding::Utf32] {
                    if let Some(change) = incremental_change(old, new, encoding).unwrap() {
                        let result = apply_edits(
                            &snapshot(old),
                            None,
                            &[TextEdit::new(change.range.unwrap(), change.text)],
                            encoding,
                        )
                        .unwrap();
                        assert_eq!(
                            result, *new,
                            "old={old:?}, new={new:?}, encoding={encoding:?}"
                        );
                    } else {
                        assert_eq!(old, new);
                    }
                }
            }
        }
    }
    #[test]
    fn preflight_rejects_stale_invalid_and_overlapping_edits() {
        let s = snapshot("a😀bc");
        let edit = TextEdit::new(
            Range::new(Position::new(0, 1), Position::new(0, 3)),
            "x".into(),
        );
        assert_eq!(
            apply_edits(&s, Some(4), &[edit.clone()], Encoding::Utf16).unwrap(),
            "axbc"
        );
        assert!(apply_edits(&s, Some(3), &[edit.clone()], Encoding::Utf16).is_err());
        assert!(apply_edits(&s, None, &[edit.clone(), edit], Encoding::Utf16).is_err());
    }
    #[test]
    fn same_position_insertions_keep_array_order() {
        let edits = [
            TextEdit::new(Range::default(), "first".into()),
            TextEdit::new(Range::default(), "second".into()),
        ];
        assert_eq!(
            apply_edits(&snapshot("tail"), None, &edits, Encoding::Utf16).unwrap(),
            "firstsecondtail"
        );
    }
    #[test]
    fn snippets_expand_defaults_choices_mirrors_and_escapes() {
        let s = snippet("fn ${1:name}(${2|a,b|}) { $1; \\$x; $0 }").unwrap();
        assert_eq!(s.text, "fn name(a) { name; $x;  }");
        assert_eq!(s.tabstops[0], (1, 3..7));
        assert_eq!(s.tabstops.last().unwrap().0, 0);
        assert_eq!(snippet("${1:a${2:b}}$0").unwrap().text, "ab");
        assert!(snippet("${1/foo/bar/}").is_err());
        assert!(snippet("${1:unclosed").is_err());
        assert!(snippet("$TM_FILENAME").is_err());
        assert!(snippet("${TM_FILENAME}").is_err());
        assert_eq!(snippet("$1:${1:value}").unwrap().text, "value:value");
    }
    #[test]
    fn snippet_backslash_escapes_only_lsp_escapable_characters() {
        assert_eq!(
            snippet(r#"re.compile(r"${1:\d+}")"#).unwrap().text,
            r#"re.compile(r"\d+")"#
        );
        assert_eq!(snippet(r"a\nb\\\}").unwrap().text, r"a\nb\}");
        assert_eq!(snippet("cost $ and $(x)").unwrap().text, "cost $ and $(x)");
        assert_eq!(snippet(r"${1:{\}}").unwrap().text, "{}");
        assert_eq!(snippet(r"${1|a\,b,c\d|}").unwrap().text, r"a,b");
    }
    #[test]
    fn workspace_edit_prefers_document_changes_over_changes() {
        let edit: WorkspaceEdit = serde_json::from_value(serde_json::json!({
            "changes":{"file:///a":[]},
            "documentChanges":[{"textDocument":{"uri":"file:///a","version":3},"edits":[]}]
        }))
        .unwrap();
        let edits = workspace_edits(edit).unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].1, Some(3));
    }
    #[test]
    fn workspace_resource_operations_rejected_before_edits() {
        let edit: WorkspaceEdit = serde_json::from_value(serde_json::json!({"documentChanges":[{"textDocument":{"uri":"file:///a","version":1},"edits":[]},{"kind":"delete","uri":"file:///b"}]})).unwrap();
        assert!(workspace_edits(edit).is_err());
    }
}
