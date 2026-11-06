//! Rich Git documents: Markdown in the house style, closed Mermaid fences as diagrams, and
//! standalone images or videos. Relative assets use the captured revision, or the working
//! tree for a Diff after-preview. Long documents are virtualized as lists of blocks.
//! GitHub's extensions are handled here: HTML comments are dropped, `<details>` folds,
//! `> [!NOTE]` alerts, `suggestion` fences, `:emoji:` and `@user`/`#123`/SHA links.
use super::mermaid::{self, Diagram};
use crate::media::{self, Kind, Lru, MediaFile, MediaView, Playing, Side, Source};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::TextViewState;
use gpui_kit::component::{ActiveTheme, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

pub(crate) fn is_markdown(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".md", ".markdown", ".mdx", ".mdown"]
        .iter()
        .any(|e| lower.ends_with(e))
}

/// Where a document's relative links point.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DocContext {
    /// The repository worktree containing the document and its assets.
    pub(crate) repo: PathBuf,
    /// The commit relative assets resolve against; `None` when unknown.
    pub(crate) rev: Option<String>,
    /// Resolve relative assets and links in the working tree instead of a Git revision.
    pub(crate) working_tree: bool,
    /// The document's directory inside the repository (`""` for the root).
    pub(crate) dir: String,
    /// `https://github.com/owner/repo`, for opening what can't be shown here.
    pub(crate) web: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Block {
    Text(String),
    Mermaid(String),
    Media {
        url: String,
        alt: String,
    },
    /// `<details>`: collapsed unless `open`; the body is blocks of its own.
    Details {
        summary: String,
        open: bool,
        body: Vec<Block>,
    },
    /// `> [!NOTE]` and friends; `kind` is the label ("Note", "Tip", …), `body` Markdown.
    Alert {
        kind: &'static str,
        body: String,
    },
    /// A ```` ```suggestion ```` fence: the lines a reviewer proposes.
    Suggestion(String),
}

/// Text blocks are cut at blank lines once they reach this size, and at line ends past `HARD`,
/// so the reader lays out only what is on screen.
const CHUNK: usize = 2048;
const HARD: usize = 8192;

fn fence(line: &str) -> Option<(u8, usize, &str)> {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let ch = *trimmed.as_bytes().first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let len = trimmed.bytes().take_while(|b| *b == ch).count();
    let info = trimmed[len..].trim();
    (len >= 3 && !(ch == b'`' && info.contains('`'))).then_some((ch, len, info))
}

fn closes(line: &str, ch: u8, len: usize) -> bool {
    fence(line).is_some_and(|(c, l, info)| c == ch && l >= len && info.is_empty())
}

/// A fence anywhere a container allows one: after blockquote `>`s, list markers or list
/// indentation. Used to keep code out of text transforms; `fence` decides blocks.
fn code_fence(line: &str) -> Option<(u8, usize, &str)> {
    let mut t = line;
    loop {
        let s = t.trim_start_matches([' ', '\t']);
        match s.strip_prefix('>').or_else(|| list_marker(s)) {
            Some(rest) => t = rest,
            None => return fence(s),
        }
    }
}

fn code_closes(line: &str, ch: u8, len: usize) -> bool {
    code_fence(line).is_some_and(|(c, l, info)| c == ch && l >= len && info.is_empty())
}

/// What follows a list marker (`-`, `*`, `+`, `1.`, `1)` and a space) at the start of `s`.
fn list_marker(s: &str) -> Option<&str> {
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    let rest = match digits {
        0 => s.strip_prefix(['-', '*', '+'])?,
        1..=9 => s[digits..].strip_prefix(['.', ')'])?,
        _ => return None,
    };
    rest.starts_with([' ', '\t']).then_some(rest)
}

/// Whether a line starts a block, so it can't lazily continue a quote's paragraph.
fn starts_block(line: &str) -> bool {
    let t = line.trim_start();
    let hashes = t.bytes().take_while(|c| *c == b'#').count();
    fence(line).is_some()
        || quoted(line).is_some()
        || list_marker(t).is_some()
        || ((1..=6).contains(&hashes) && t[hashes..].chars().next().is_none_or(char::is_whitespace))
        || html_block(line, false).is_some()
        || thematic_or_setext(t.trim_end())
}

/// `---`, `* * *`, `___` (3+ of one character, spaces allowed) or a setext `===` underline.
fn thematic_or_setext(t: &str) -> bool {
    let Some(c) = t.chars().next() else {
        return false;
    };
    let marks = t.chars().filter(|x| *x == c).count();
    t.chars().all(|x| x == c || x == ' ' || x == '\t')
        && match c {
            '-' | '*' | '_' => marks >= 3,
            '=' => !t.contains([' ', '\t']),
            _ => false,
        }
}

/// Splits Markdown into blocks. Fences stay whole; an unclosed fence runs to the end. With
/// `chunk`, long text is also cut into pieces.
pub(crate) fn blocks(source: &str, chunk: bool) -> Vec<Block> {
    split(source, chunk, None)
}

/// [`blocks`], with a details body given the whole document's reference definitions.
fn split(source: &str, chunk: bool, doc_refs: Option<&Refs>) -> Vec<Block> {
    let source = strip_comments(source);
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    // Link reference definitions apply document-wide; each block gets a copy of those it uses
    // (they render nothing), so `[text][1]` still resolves after a split.
    let own;
    let refs = match doc_refs {
        Some(refs) => refs,
        None => {
            own = definitions(&lines);
            &own
        }
    };
    let mut out = vec![];
    let mut text = String::new();
    let flush = |text: &mut String, out: &mut Vec<Block>| {
        if !text.trim().is_empty() {
            let mut block = std::mem::take(text);
            let used = used_refs(&block, refs);
            if !used.is_empty() {
                block = format!("{}\n\n{used}", block.trim_end());
            }
            out.push(Block::Text(block));
        }
        text.clear();
    };
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some((ch, len, info)) = fence(line) {
            let close = (i + 1..lines.len()).find(|&j| closes(lines[j], ch, len));
            let lang = info.split_whitespace().next().unwrap_or("");
            match close {
                Some(j) if lang.eq_ignore_ascii_case("mermaid") => {
                    flush(&mut text, &mut out);
                    out.push(Block::Mermaid(lines[i + 1..j].concat()));
                    i = j + 1;
                }
                Some(j) if lang.eq_ignore_ascii_case("suggestion") => {
                    flush(&mut text, &mut out);
                    out.push(Block::Suggestion(lines[i + 1..j].concat()));
                    i = j + 1;
                }
                Some(j) => {
                    text.push_str(&lines[i..=j].concat());
                    i = j + 1;
                }
                None => {
                    text.push_str(&lines[i..].concat());
                    break;
                }
            }
            continue;
        }
        // A fence inside a list or quote is code too: no media, details or alerts in it.
        if let Some((ch, len, _)) = code_fence(line) {
            let end = (i + 1..lines.len())
                .find(|&j| code_closes(lines[j], ch, len))
                .map_or(lines.len(), |j| j + 1);
            text.push_str(&lines[i..end].concat());
            i = end;
            continue;
        }
        if let Some((summary, open, body, used, tail)) = details(&lines[i..]) {
            flush(&mut text, &mut out);
            out.push(Block::Details {
                summary,
                open,
                body: split(&body, chunk, Some(refs)),
            });
            text.push_str(&tail);
            i += used;
            continue;
        }
        // An alert opens a blockquote; a `[!NOTE]` inside one is just text.
        if let Some(kind) = alert_kind(line)
            && !text.lines().last().is_some_and(|l| quoted(l).is_some())
        {
            flush(&mut text, &mut out);
            let mut body = String::new();
            i += 1;
            while let Some(line) = lines.get(i) {
                if let Some(rest) = quoted(line) {
                    body.push_str(rest);
                } else if !line.trim().is_empty()
                    && body.lines().last().is_some_and(|l| !l.trim().is_empty())
                    && !starts_block(line)
                {
                    // A lazy continuation of the quote's paragraph.
                    body.push_str(line);
                } else {
                    break;
                }
                i += 1;
            }
            let used = used_refs(&body, refs);
            if !used.is_empty() {
                body = format!("{}\n\n{used}", body.trim_end());
            }
            out.push(Block::Alert { kind, body });
            continue;
        }
        if let Some(media) = media_line(line) {
            flush(&mut text, &mut out);
            out.extend(media);
            i += 1;
            continue;
        }
        // A line past HARD (one huge paragraph, minified text) is cut at spaces or char
        // boundaries, since a block is laid out whole.
        if chunk && line.len() > HARD {
            flush(&mut text, &mut out);
            let mut rest = line;
            while !rest.is_empty() {
                let at = rest.floor_char_boundary(HARD);
                let at = match rest[..at].rfind(' ') {
                    Some(sp) if at < rest.len() => sp + 1,
                    _ => at,
                };
                text.push_str(&rest[..at]);
                flush(&mut text, &mut out);
                rest = &rest[at..];
            }
            i += 1;
            continue;
        }
        text.push_str(line);
        if chunk && text.len() >= CHUNK {
            let next_plain = lines.get(i + 1).is_none_or(|n| !continues(n));
            // Past HARD, a paragraph may end at a line break, but tables, lists, quotes and
            // indented code stay whole.
            let hard = text.len() >= HARD && next_plain && !continues(line);
            if (line.trim().is_empty() && next_plain) || hard {
                flush(&mut text, &mut out);
            }
        }
        i += 1;
    }
    flush(&mut text, &mut out);
    out
}

/// Link reference definition lines by [`label_key`].
type Refs = HashMap<String, String>;

/// A reference label as CommonMark matches it: whitespace collapsed, case folded.
fn label_key(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The link reference definitions outside code; the first for a label wins.
fn definitions(lines: &[&str]) -> Refs {
    let mut refs = Refs::new();
    let mut in_fence = None;
    for line in lines {
        match (in_fence, code_fence(line)) {
            (Some((ch, len)), _) if code_closes(line, ch, len) => in_fence = None,
            (Some(_), _) => {}
            (None, Some((ch, len, _))) => in_fence = Some((ch, len)),
            (None, None) => {
                if let Some(label) = reference(line) {
                    let def = line.trim_end().to_string();
                    refs.entry(label_key(label)).or_insert(def);
                }
            }
        }
    }
    refs
}

/// The definitions `md` uses (`[label]`, `[text][label]`, `[label][]`) and doesn't have
/// itself, one per line. Copying only these keeps a split document linear in size.
fn used_refs(md: &str, refs: &Refs) -> String {
    let mut out = String::new();
    if refs.is_empty() {
        return out;
    }
    let mut seen: HashSet<String> = md.lines().filter_map(reference).map(label_key).collect();
    let mut rest = md;
    while let Some(open) = rest.find('[') {
        rest = &rest[open + 1..];
        let Some(end) = rest.find(['[', ']']) else {
            break;
        };
        if rest.as_bytes()[end] == b']' {
            let key = label_key(&rest[..end]);
            if let Some(def) = refs.get(&key)
                && seen.insert(key)
            {
                out.push_str(def);
                out.push('\n');
            }
        }
        rest = &rest[end..];
    }
    out
}

/// Whether `line` is indented code: 4+ columns after a blank line or continuing indented
/// code, unless it continues a list item. `state` is (indented, in a list), carried between
/// calls; call it for lines outside fences and HTML blocks.
fn indented_code(line: &str, after_blank: bool, state: &mut (bool, bool)) -> bool {
    let (indented, in_list) = state;
    let blank = line.trim().is_empty();
    let mut width = 0;
    for c in line.chars() {
        match c {
            ' ' => width += 1,
            '\t' => width += 4 - width % 4,
            _ => break,
        }
    }
    let wide = width >= 4;
    if !blank && !wide {
        *in_list = list_marker(line.trim_start()).is_some();
    }
    if !blank {
        *indented = wide && !*in_list && (after_blank || *indented);
    }
    *indented
}

/// `source` without HTML comments outside code. Bots keep state in comments (often huge
/// base64), so this runs before splitting. As on GitHub, a `<!--` opening a line (an HTML
/// block) hides everything up to `-->` or the end; elsewhere it is inline and must close
/// within its paragraph, or it stays as text.
fn strip_comments(source: &str) -> Cow<'_, str> {
    if !source.contains("<!--") {
        return Cow::Borrowed(source);
    }
    let mut out = String::with_capacity(source.len());
    let mut in_fence = None;
    let mut rest = source;
    // Whether `rest` resumes a line after a comment, so nothing in it opens a line.
    let mut mid = false;
    let (mut after_blank, mut indented) = (true, (false, false));
    while !rest.is_empty() {
        let line = &rest[..rest.find('\n').map_or(rest.len(), |n| n + 1)];
        let code = in_fence.is_none() && !mid && indented_code(line, after_blank, &mut indented);
        match (in_fence, code_fence(line)) {
            _ if code => {}
            (Some((ch, len)), _) => {
                if code_closes(line, ch, len) {
                    in_fence = None;
                }
            }
            (None, Some((ch, len, _))) => in_fence = Some((ch, len)),
            (None, None) => {
                if let Some(at) = outside_code(line, "<!--") {
                    let after = &rest[at + 4..];
                    let block = !mid && at <= 3 && line[..at].bytes().all(|c| c == b' ');
                    let end = after.find("-->").filter(|e| {
                        // Inline, no blank line may come before the end.
                        let span = &after[..*e];
                        let whole = span.rsplit_once('\n').map_or("", |(lines, _)| lines);
                        block || !whole.split('\n').skip(1).any(|l| l.trim().is_empty())
                    });
                    match end {
                        Some(e) => {
                            out.push_str(&line[..at]);
                            rest = &after[e + 3..];
                        }
                        None if block => {
                            out.push_str(&line[..at]);
                            rest = "";
                        }
                        None => {
                            out.push_str(&line[..at + 4]);
                            rest = after;
                        }
                    }
                    mid = true;
                    continue;
                }
            }
        }
        out.push_str(line);
        rest = &rest[line.len()..];
        mid = false;
        after_blank = line.trim().is_empty();
    }
    Cow::Owned(out)
}

/// Where `needle` first occurs in `line` outside inline code spans.
fn outside_code(line: &str, needle: &str) -> Option<usize> {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'`' {
            i = code_span_end(b, i);
        } else if line.as_bytes()[i..].starts_with(needle.as_bytes()) {
            return Some(i);
        } else {
            i += 1;
        }
    }
    None
}

/// The end of the code span opening at `b[i]` (a backtick), or just past the backticks when
/// they don't close on this line and so are literal.
fn code_span_end(b: &[u8], i: usize) -> usize {
    let n = b[i..].iter().take_while(|c| **c == b'`').count();
    let mut j = i + n;
    while j < b.len() {
        let m = b[j..].iter().take_while(|c| **c == b'`').count();
        if m == n {
            return j + m;
        }
        j += m.max(1);
    }
    i + n
}

/// The index of the next `<details` tag in ASCII-lowercased `lower` at or after `from`,
/// outside inline code.
fn details_tag(lower: &str, from: usize) -> Option<usize> {
    let mut from = from;
    while let Some(at) = outside_code(&lower[from..], "<details") {
        let at = from + at;
        let next = lower.as_bytes().get(at + 8);
        if next.is_none_or(|c| matches!(c, b'>' | b'/' | b' ' | b'\t' | b'\r' | b'\n')) {
            return Some(at);
        }
        from = at + 8;
    }
    None
}

/// A `<details>` element opening `lines[0]`: its summary, `open` attribute, body, the number
/// of lines it spans and any text after `</details>`. Nested elements stay in the body; fences
/// in it are skipped. An unclosed element runs to the end, as in a browser.
fn details(lines: &[&str]) -> Option<(String, bool, String, usize, String)> {
    let first = lines[0].trim_start();
    let lower = first.to_ascii_lowercase();
    if details_tag(&lower, 0) != Some(0) {
        return None;
    }
    let gt = first.find('>')?;
    let open = lower[8..gt]
        .split(|c: char| c.is_whitespace() || c == '/')
        .any(|a| a == "open" || a.starts_with("open="));
    let (mut body, mut tail, mut used) = (String::new(), String::new(), lines.len());
    let (mut depth, mut in_fence) = (1, None);
    let segments = std::iter::once(&first[gt + 1..]).chain(lines[1..].iter().copied());
    'lines: for (n, seg) in segments.enumerate() {
        if n > 0 {
            match (in_fence, code_fence(seg)) {
                (Some((ch, len)), _) => {
                    if code_closes(seg, ch, len) {
                        in_fence = None;
                    }
                    body.push_str(seg);
                    continue;
                }
                (None, Some((ch, len, _))) => {
                    in_fence = Some((ch, len));
                    body.push_str(seg);
                    continue;
                }
                _ => {}
            }
        }
        let lower = seg.to_ascii_lowercase();
        let mut from = 0;
        loop {
            let close = outside_code(&lower[from..], "</details>").map(|c| from + c);
            match (details_tag(&lower, from), close) {
                (Some(o), c) if c.is_none_or(|c| o < c) => {
                    depth += 1;
                    from = o + 8;
                }
                (_, Some(c)) => {
                    depth -= 1;
                    from = c + 10;
                    if depth == 0 {
                        body.push_str(&seg[..c]);
                        tail = seg[from..].trim_start().to_string();
                        used = n + 1;
                        break 'lines;
                    }
                }
                _ => break,
            }
        }
        body.push_str(seg);
    }
    let mut summary = String::from("Details");
    let trimmed = body.trim_start();
    if trimmed
        .get(..8)
        .is_some_and(|p| p.eq_ignore_ascii_case("<summary"))
        && let Some(gt) = trimmed.find('>')
        && let Some(end) = trimmed.to_ascii_lowercase().find("</summary>")
        && end > gt
    {
        let text = plain(&trimmed[gt + 1..end]);
        if !text.is_empty() {
            summary = enrich(&text, None);
        }
        body = trimmed[end + 10..].to_string();
    }
    Some((summary, open, body, used, tail))
}

/// Inline HTML as plain text: tags dropped, common entities decoded, whitespace collapsed.
fn plain(html: &str) -> String {
    let mut text = String::new();
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        text.push_str(&rest[..lt]);
        rest = rest[lt..].find('>').map_or("", |gt| &rest[lt + gt + 1..]);
    }
    text.push_str(rest);
    let text = text
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&");
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A blockquote line's content after `>` and one optional space.
fn quoted(line: &str) -> Option<&str> {
    let t = line.trim_start_matches(' ');
    let rest = (line.len() - t.len() <= 3)
        .then_some(t)?
        .strip_prefix('>')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

/// `> [!NOTE]` (any case, alone on its line) as the alert's label.
fn alert_kind(line: &str) -> Option<&'static str> {
    let marker = quoted(line)?.trim().strip_prefix("[!")?.strip_suffix(']')?;
    ["Note", "Tip", "Important", "Warning", "Caution"]
        .into_iter()
        .find(|k| k.eq_ignore_ascii_case(marker))
}

/// A word character, for link boundaries.
fn word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// For a link (`[text](url)` or `[text][ref]`) opening at `b[i]`: the index of the `]` ending
/// its text, and its end.
fn link_end(b: &[u8], i: usize) -> Option<(usize, usize)> {
    let close = |open: u8, shut: u8, from: usize| {
        let mut depth = 0;
        for (j, c) in b.iter().enumerate().skip(from) {
            if *c == open {
                depth += 1;
            } else if *c == shut {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
        }
        None
    };
    let text = close(b'[', b']', i)?;
    let end = match b.get(text + 1)? {
        b'(' => close(b'(', b')', text + 1)?,
        b'[' => close(b'[', b']', text + 1)?,
        _ => return None,
    };
    Some((text, end + 1))
}

/// Block-level tag names that open a CommonMark HTML block (type 6).
const BLOCK_TAGS: &[&str] = &[
    "address",
    "article",
    "aside",
    "base",
    "basefont",
    "blockquote",
    "body",
    "caption",
    "center",
    "col",
    "colgroup",
    "dd",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "frame",
    "frameset",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hr",
    "html",
    "iframe",
    "legend",
    "li",
    "link",
    "main",
    "menu",
    "menuitem",
    "nav",
    "noframes",
    "ol",
    "optgroup",
    "option",
    "p",
    "param",
    "search",
    "section",
    "summary",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "title",
    "tr",
    "track",
    "ul",
];

/// Whether `line` opens a CommonMark HTML block, which is rendered as HTML with no Markdown
/// inside. Returns what ends it: a line containing the string, or `""` for a blank line.
/// A lone tag (type 7) can't interrupt a paragraph, hence `after_blank`.
fn html_block(line: &str, after_blank: bool) -> Option<&'static str> {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() > 3 || !t.starts_with('<') {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    let tag_ends = |rest: &str| {
        rest.is_empty() || rest.starts_with([' ', '\t', '\r', '\n', '>']) || rest.starts_with("/>")
    };
    for (open, end) in [
        ("<script", "</script>"),
        ("<pre", "</pre>"),
        ("<style", "</style>"),
        ("<textarea", "</textarea>"),
    ] {
        if lower.strip_prefix(open).is_some_and(tag_ends) {
            return Some(end);
        }
    }
    for (open, end) in [("<!--", "-->"), ("<?", "?>"), ("<![cdata[", "]]>")] {
        if lower.starts_with(open) {
            return Some(end);
        }
    }
    if lower[1..].starts_with('!') && lower[2..].starts_with(|c: char| c.is_ascii_alphabetic()) {
        return Some(">");
    }
    let name = lower.strip_prefix("</").unwrap_or(&lower[1..]);
    let n = name
        .bytes()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == b'-')
        .count();
    if n == 0 || !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    if tag_ends(&name[n..]) && BLOCK_TAGS.contains(&&name[..n]) {
        return Some("");
    }
    let whole = t.trim_end();
    (after_blank && whole.ends_with('>') && !whole[1..].contains('<')).then_some("")
}

/// Common `:shortcode:`s; a small table rather than a dependency.
const EMOJI: &[(&str, &str)] = &[
    ("+1", "👍"),
    ("-1", "👎"),
    ("100", "💯"),
    ("art", "🎨"),
    ("bell", "🔔"),
    ("blush", "😊"),
    ("books", "📚"),
    ("boom", "💥"),
    ("bug", "🐛"),
    ("bulb", "💡"),
    ("clap", "👏"),
    ("clipboard", "📋"),
    ("confused", "😕"),
    ("construction", "🚧"),
    ("cry", "😢"),
    ("eyes", "👀"),
    ("fire", "🔥"),
    ("gear", "⚙️"),
    ("grin", "😁"),
    ("hammer", "🔨"),
    ("heart", "❤️"),
    ("heavy_check_mark", "✔️"),
    ("heavy_minus_sign", "➖"),
    ("heavy_plus_sign", "➕"),
    ("hourglass", "⌛"),
    ("information_source", "ℹ️"),
    ("joy", "😂"),
    ("label", "🏷️"),
    ("laughing", "😆"),
    ("link", "🔗"),
    ("lock", "🔒"),
    ("mag", "🔍"),
    ("memo", "📝"),
    ("muscle", "💪"),
    ("no_entry", "⛔"),
    ("ok_hand", "👌"),
    ("package", "📦"),
    ("pencil", "📝"),
    ("pencil2", "✏️"),
    ("point_right", "👉"),
    ("pray", "🙏"),
    ("pushpin", "📌"),
    ("question", "❓"),
    ("raised_hands", "🙌"),
    ("recycle", "♻️"),
    ("red_circle", "🔴"),
    ("robot", "🤖"),
    ("rocket", "🚀"),
    ("rotating_light", "🚨"),
    ("see_no_evil", "🙈"),
    ("smile", "😄"),
    ("smiley", "😃"),
    ("sob", "😭"),
    ("sparkles", "✨"),
    ("speech_balloon", "💬"),
    ("star", "⭐"),
    ("stop_sign", "🛑"),
    ("sweat_smile", "😅"),
    ("tada", "🎉"),
    ("thinking", "🤔"),
    ("thumbsdown", "👎"),
    ("thumbsup", "👍"),
    ("warning", "⚠️"),
    ("wave", "👋"),
    ("white_check_mark", "✅"),
    ("wink", "😉"),
    ("wrench", "🔧"),
    ("x", "❌"),
    ("zap", "⚡"),
];

/// Images inside text as their alt text, outside code: the Markdown renderer doesn't load
/// them and leaves a blank gap. `<picture>` and `<source>` tags go, so a picture becomes its
/// `<img>`'s alt. Images alone on a line are media blocks before this runs.
fn inline_images(md: &str) -> Cow<'_, str> {
    let lower = md.to_ascii_lowercase();
    if !["<img", "<picture", "<source"]
        .iter()
        .any(|t| lower.contains(t))
    {
        return Cow::Borrowed(md);
    }
    let mut out = String::with_capacity(md.len());
    let mut in_fence = None;
    for line in md.split_inclusive('\n') {
        match (in_fence, code_fence(line)) {
            (Some((ch, len)), _) => {
                if code_closes(line, ch, len) {
                    in_fence = None;
                }
                out.push_str(line);
                continue;
            }
            (None, Some((ch, len, _))) => {
                in_fence = Some((ch, len));
                out.push_str(line);
                continue;
            }
            _ => {}
        }
        let b = line.as_bytes();
        let (mut i, mut done) = (0, 0);
        while i < b.len() {
            if b[i] == b'`' {
                i = code_span_end(b, i);
                continue;
            }
            if b[i] == b'<' {
                let at = i + 1 + usize::from(b.get(i + 1) == Some(&b'/'));
                let n = b[at..]
                    .iter()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .count();
                let name = &line[at..at + n];
                if ["img", "picture", "source"]
                    .iter()
                    .any(|t| t.eq_ignore_ascii_case(name))
                    && let Some(gt) = line[i..].find('>')
                {
                    out.push_str(&line[done..i]);
                    if name.eq_ignore_ascii_case("img") {
                        out.push_str(attr(&line[i..i + gt], "alt").unwrap_or_default().trim());
                    }
                    i += gt + 1;
                    done = i;
                    continue;
                }
            }
            i += 1;
        }
        out.push_str(&line[done..]);
    }
    Cow::Owned(out)
}

/// GitHub's inline extensions outside code, links, HTML and URLs: `:shortcode:` emoji and,
/// given the repository's web URL, `@user`, `#123`, `owner/repo#123` and commit SHAs as
/// links.
fn enrich(md: &str, web: Option<&str>) -> String {
    let web = web.map(|w| w.trim_end_matches('/'));
    let origin = web.map(|w| w.split('/').take(3).collect::<Vec<_>>().join("/"));
    let links = web.zip(origin.as_deref());
    let mut out = String::with_capacity(md.len());
    let (mut in_fence, mut html, mut after_blank) = (None, None::<&str>, true);
    let mut indented = (false, false);
    for line in md.split_inclusive('\n') {
        let blank = line.trim().is_empty();
        if in_fence.is_none() && html.is_none() && indented_code(line, after_blank, &mut indented) {
            out.push_str(line);
            after_blank = blank;
            continue;
        }
        // Code and HTML blocks pass through unchanged.
        let ends = |end: &str| {
            if end.is_empty() {
                blank
            } else {
                line.to_ascii_lowercase().contains(end)
            }
        };
        match (in_fence, html, code_fence(line)) {
            (Some((ch, len)), ..) => {
                if code_closes(line, ch, len) {
                    in_fence = None;
                }
                out.push_str(line);
            }
            (None, Some(end), _) => {
                if ends(end) {
                    html = None;
                }
                out.push_str(line);
            }
            (None, None, Some((ch, len, _))) => {
                in_fence = Some((ch, len));
                out.push_str(line);
            }
            _ => match html_block(line, after_blank) {
                Some(end) => {
                    // The end may be on the opening line (`<pre>…</pre>`), but not its start.
                    let rest = line.get(2..).unwrap_or("");
                    let done = if end.is_empty() {
                        false
                    } else {
                        rest.to_ascii_lowercase().contains(end)
                    };
                    html = (!done).then_some(end);
                    out.push_str(line);
                }
                None if reference(line).is_some() => out.push_str(line),
                None => enrich_line(line, links, &mut out),
            },
        }
        after_blank = blank;
    }
    out
}

/// One line of [`enrich`]; `links` is the repository's web URL and its origin.
fn enrich_line(line: &str, links: Option<(&str, &str)>, out: &mut String) {
    let b = line.as_bytes();
    let run = |from: usize, ok: fn(u8) -> bool| {
        from + b[from.min(b.len())..]
            .iter()
            .take_while(|c| ok(**c))
            .count()
    };
    let ends = |at: usize| !b.get(at).is_some_and(|c| word(*c));
    let (mut i, mut done) = (0, 0);
    while i < b.len() {
        let prev = if i == 0 { b' ' } else { b[i - 1] };
        let start = !word(prev) && !b"/.-@#&".contains(&prev);
        // A replacement for `line[i..end]`.
        let mut hit: Option<(usize, String)> = None;
        match (b[i], links) {
            (b'`', _) => {
                i = code_span_end(b, i);
                continue;
            }
            (b'[', _) => {
                // Link text gets emoji only; the destination stays as written.
                if let Some((text, e)) = link_end(b, i) {
                    out.push_str(&line[done..=i]);
                    enrich_line(&line[i + 1..text], None, out);
                    (i, done) = (e, text);
                    continue;
                }
            }
            (b'<', _)
                if b.get(i + 1)
                    .is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, b'/' | b'!')) =>
            {
                if let Some(e) = line[i..].find('>') {
                    // Inline code, preformatted text, keys and links skip to their closing tag.
                    let lower = line[i..].to_ascii_lowercase();
                    let n = lower[1..]
                        .bytes()
                        .take_while(u8::is_ascii_alphanumeric)
                        .count();
                    let name = &lower[1..1 + n];
                    i = match ["a", "code", "kbd", "pre"].contains(&name) {
                        true => lower
                            .find(&format!("</{name}>"))
                            .map_or(i + e + 1, |c| i + c + n + 3),
                        false => i + e + 1,
                    };
                    continue;
                }
            }
            (b':', _) => {
                let e = run(i + 1, |c| c.is_ascii_alphanumeric() || b"_+-".contains(&c));
                if b.get(e) == Some(&b':')
                    && let Some((_, emoji)) = EMOJI.iter().find(|(k, _)| *k == &line[i + 1..e])
                {
                    hit = Some((e + 1, emoji.to_string()));
                }
            }
            (b'@', Some((_, origin))) if start => {
                let e = run(i + 1, |c| c.is_ascii_alphanumeric() || c == b'-');
                let name = &line[i + 1..e];
                if (1..=39).contains(&name.len())
                    && !name.starts_with('-')
                    && !name.ends_with('-')
                    && ends(e)
                    && b.get(e) != Some(&b'/')
                {
                    hit = Some((e, format!("[@{name}]({origin}/{name})")));
                }
            }
            (b'#', Some((web, _))) if start => {
                let e = run(i + 1, |c| c.is_ascii_digit());
                if e > i + 1 && ends(e) {
                    let n = &line[i + 1..e];
                    hit = Some((e, format!("[#{n}]({web}/issues/{n})")));
                }
            }
            (c, links) if start && word(c) => {
                let rest = &line[i..];
                if ["http://", "https://", "www."]
                    .iter()
                    .any(|p| rest.starts_with(p))
                {
                    i = run(i, |c| !c.is_ascii_whitespace() && c != b'<');
                    continue;
                }
                let w = run(i, word);
                if let Some((web, origin)) = links {
                    let owner = run(i, |c| c.is_ascii_alphanumeric() || c == b'-');
                    let repo = run(owner + 1, |c| {
                        c.is_ascii_alphanumeric() || b"._-".contains(&c)
                    });
                    let num = run(repo + 1, |c| c.is_ascii_digit());
                    let sha = &line[i..w];
                    if owner > i
                        && b.get(owner) == Some(&b'/')
                        && repo > owner + 1
                        && b.get(repo) == Some(&b'#')
                        && num > repo + 1
                        && ends(num)
                    {
                        let (path, n) = (&line[i..repo], &line[repo + 1..num]);
                        hit = Some((num, format!("[{path}#{n}]({origin}/{path}/issues/{n})")));
                    } else if (7..=40).contains(&sha.len())
                        && sha.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
                        && sha.bytes().any(|c| c.is_ascii_digit())
                        && sha.bytes().any(|c| c.is_ascii_alphabetic())
                    {
                        hit = Some((w, format!("[`{}`]({web}/commit/{sha})", &sha[..7])));
                    }
                }
                if hit.is_none() {
                    i = w;
                    continue;
                }
            }
            _ => {}
        }
        match hit {
            Some((end, with)) => {
                out.push_str(&line[done..i]);
                out.push_str(&with);
                (i, done) = (end, end);
            }
            None => i += 1,
        }
    }
    out.push_str(&line[done..]);
}

/// A compact preview of `body`: comments stripped, cut at a block boundary within `max_bytes`
/// (a fence is never left open), and whether anything was cut.
pub fn preview(body: &str, max_bytes: usize) -> (String, bool) {
    let body = strip_comments(body);
    let body = body.trim_end();
    if body.len() <= max_bytes {
        return (body.to_string(), false);
    }
    // Ends of lines, and of paragraphs (blank lines, closing fences), outside fences.
    let (mut line_end, mut para, mut in_fence, mut at) = (None, None, None, 0);
    for line in body.split_inclusive('\n') {
        let end = at + line.len();
        if end > max_bytes {
            break;
        }
        match (in_fence, fence(line)) {
            (Some((ch, len, _)), _) if closes(line, ch, len) => {
                in_fence = None;
                (line_end, para) = (Some(end), Some(end));
            }
            (Some(_), _) => {}
            (None, Some((ch, len, _))) => in_fence = Some((ch, len, at)),
            (None, None) => {
                line_end = Some(end);
                if line.trim().is_empty() {
                    para = Some(end);
                }
            }
        }
        at = end;
    }
    // A cut that keeps only blank lines is no preview.
    let shows = |c: &usize| !body[..*c].trim().is_empty();
    let cut = (para.filter(|p| *p >= max_bytes / 2).filter(shows)).or(line_end.filter(shows));
    let text = match (cut, in_fence) {
        (Some(c), _) => body[..c].trim_end().to_string(),
        // The budget ends inside a fence that opens the body: close it after its last whole
        // line.
        (None, Some((ch, len, start))) if at > start => {
            let close = (ch as char).to_string().repeat(len);
            format!("{}{close}", &body[..at])
        }
        // One long first line, cut at a space; a budget smaller than its first word keeps
        // that word.
        _ => {
            let at = body.floor_char_boundary(max_bytes);
            let at = body[..at].rfind(' ').filter(shows).unwrap_or(at);
            match body[..at].trim_end() {
                text if !text.trim().is_empty() => text.to_string(),
                _ => body
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_string(),
            }
        }
    };
    (text, true)
}

/// Whether a line continues a structure that must not be split from the line before.
fn continues(line: &str) -> bool {
    let t = line.trim_start();
    line.starts_with([' ', '\t'])
        || t.starts_with(['|', '>', '-', '*', '+'])
        || t.split_once(['.', ')']).is_some_and(|(n, _)| {
            !n.is_empty() && n.len() < 10 && n.bytes().all(|b| b.is_ascii_digit())
        })
}

/// The label of `[label]: destination`, but not of a footnote, which renders visibly.
fn reference(line: &str) -> Option<&str> {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() > 3 || !t.starts_with('[') || t.starts_with("[^") {
        return None;
    }
    let (l, d) = t.split_once("]:")?;
    (l.len() > 1 && !d.trim().is_empty()).then(|| &l[1..])
}

/// An `https` URL to an image, video or GitHub attachment.
fn embeds(url: &str) -> bool {
    url.starts_with("https://")
        && !url.contains(char::is_whitespace)
        && (media::fetch::attachment(url) || media::fetch::url_kind(url).is_some())
}

/// Images on a line of their own: `![alt](url)`, `<img src="…">`, or a bare media or GitHub
/// attachment URL. `data:` images stay inline Markdown.
fn media_line(line: &str) -> Option<Vec<Block>> {
    if line.starts_with("    ") || line.starts_with('\t') {
        return None;
    }
    let mut rest = line.trim();
    if rest.is_empty() {
        return None;
    }
    // A Markdown link alone on its line, to an image, video or GitHub attachment.
    if let Some(r) = rest.strip_prefix('[')
        && !r.starts_with('!')
        && let Some((text, r)) = r.split_once("](")
        && let Some(url) = r.strip_suffix(')')
        && embeds(url)
    {
        return Some(vec![Block::Media {
            url: url.into(),
            alt: text.into(),
        }]);
    }
    let bare = rest.trim_start_matches('<').trim_end_matches('>');
    if embeds(bare) {
        return Some(vec![Block::Media {
            url: bare.into(),
            alt: String::new(),
        }]);
    }
    let mut out = vec![];
    while !rest.is_empty() {
        let (url, alt, after) = if let Some(r) = rest.strip_prefix("![") {
            let (alt, r) = r.split_once("](")?;
            let end = r.find(')')?;
            let target = r[..end].trim();
            let url = target.split_whitespace().next()?.trim_matches(['<', '>']);
            (url.to_string(), alt.to_string(), &r[end + 1..])
        } else if rest
            .get(..4)
            .is_some_and(|p| p.eq_ignore_ascii_case("<img"))
        {
            let end = rest.find('>')?;
            let tag = &rest[..end];
            (
                attr(tag, "src")?,
                attr(tag, "alt").unwrap_or_default(),
                &rest[end + 1..],
            )
        } else {
            return None;
        };
        if url.is_empty() || url.starts_with("data:") {
            return None;
        }
        out.push(Block::Media { url, alt });
        rest = after.trim_start();
    }
    Some(out)
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find(name) {
        let at = from + at;
        from = at + name.len();
        let before = lower[..at].chars().last();
        if !before.is_some_and(char::is_whitespace) {
            continue;
        }
        let r = tag[from..].trim_start().strip_prefix('=')?.trim_start();
        let q = r.chars().next()?;
        return Some(match q {
            '"' | '\'' => r[1..].split(q).next()?.to_string(),
            _ => r
                .split(|c: char| c.is_whitespace() || c == '>')
                .next()?
                .to_string(),
        });
    }
    None
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A repository path for a relative link, or `None` when it leaves the repository.
pub(crate) fn repo_path(dir: &str, url: &str) -> Option<String> {
    let url = decode(url.split(['?', '#']).next().unwrap_or(url));
    let joined = match url.strip_prefix('/') {
        Some(root) => root.to_string(),
        None if dir.is_empty() => url,
        None => format!("{dir}/{url}"),
    };
    let mut parts: Vec<&str> = vec![];
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// What one media block shows.
fn resolve(url: &str, ctx: Option<&DocContext>) -> Result<MediaFile, String> {
    if url.contains("://") || url.starts_with("//") {
        let fetched = media::fetch::fetch(url)?;
        return Ok(MediaFile {
            path: fetched.name.into(),
            kind: fetched.kind,
            old: None,
            new: Some(fetched.side),
        });
    }
    let ctx = ctx.ok_or("relative links need a repository")?;
    let path = repo_path(&ctx.dir, url).ok_or("the link leaves the repository")?;
    let kind = media::kind(&path).ok_or("not an image or video")?;
    if ctx.working_tree {
        let root = ctx.repo.canonicalize().map_err(|e| e.to_string())?;
        let abs = root.join(&path).canonicalize().map_err(|e| e.to_string())?;
        if !abs.starts_with(&root) {
            return Err("the link leaves the repository".into());
        }
        let sha = crate::git::git(
            &ctx.repo,
            &[
                "hash-object",
                &format!("--path={path}"),
                "--",
                &abs.to_string_lossy(),
            ],
        )?;
        return Ok(MediaFile {
            // Working media paths include a repo prefix, removed by media::prepare.
            path: format!("working/{path}").into(),
            kind,
            old: None,
            new: Some(Side {
                sha,
                repo: ctx.repo.clone(),
                source: Source::Working(abs),
            }),
        });
    }
    let rev = ctx
        .rev
        .as_deref()
        .ok_or("the document's revision is unknown")?;
    let blob = crate::git::history::blob_at(&ctx.repo, rev, &path)
        .map_err(|_| "this revision isn't available locally".to_string())?
        .ok_or("not found at this revision")?;
    if blob.symlink() {
        return Err("a symbolic link; not followed".into());
    }
    Ok(MediaFile {
        path: path.into(),
        kind,
        old: None,
        new: Some(Side {
            sha: blob.id,
            repo: ctx.repo.clone(),
            source: Source::Blob,
        }),
    })
}

/// Where Open in browser goes for a block's URL.
fn web_link(url: &str, ctx: Option<&DocContext>) -> Option<String> {
    if url.starts_with("https://") || url.starts_with("http://") {
        return Some(url.into());
    }
    let ctx = ctx?;
    let path = repo_path(&ctx.dir, url)?;
    let rev = ctx.rev.as_deref().unwrap_or("HEAD");
    // Segments are percent-encoded: `a#b.png` must not become a fragment.
    let mut link = url::Url::parse(ctx.web.as_deref()?).ok()?;
    (link.path_segments_mut().ok()?)
        .pop_if_empty()
        .extend(["blob", rev])
        .extend(path.split('/'));
    Some(link.into())
}

/// What a block shows, made when it first renders.
enum View {
    /// A diagram being laid out or a file being fetched.
    Loading,
    Text(Entity<TextViewState>),
    Diagram(Diagram),
    /// A diagram that failed to render, with its source as a code fence.
    Unrendered(String, Entity<TextViewState>),
    Media(Entity<MediaView>),
    /// A file that can't be shown.
    Failed(String),
}

/// One block of the flattened document: a `Details` item is followed by its body's items.
struct Item {
    /// For `Details`, the body is moved into the following items.
    block: Block,
    /// `None` until the block first renders.
    view: Option<View>,
    /// The enclosing `Details` item; the item shows only while every ancestor is open.
    parent: Option<usize>,
    depth: usize,
    /// Whether a `Details` item is expanded.
    open: bool,
}

fn flatten(blocks: Vec<Block>, parent: Option<usize>, depth: usize, out: &mut Vec<Item>) {
    for block in blocks {
        let ix = out.len();
        let (block, body, open) = match block {
            Block::Details {
                summary,
                open,
                body,
            } => (
                Block::Details {
                    summary,
                    open,
                    body: vec![],
                },
                body,
                open,
            ),
            block => (block, vec![], false),
        };
        out.push(Item {
            block,
            view: None,
            parent,
            depth,
            open,
        });
        flatten(body, Some(ix), depth + 1, out);
    }
}

/// Where a clicked link goes: URLs pass through, repository-relative links open on GitHub at
/// the document's revision, and in-page anchors go nowhere.
fn link_url(url: &str, ctx: Option<&DocContext>) -> Option<String> {
    if url.starts_with('#') {
        return None;
    }
    if url.contains("://") || url.starts_with("mailto:") {
        return Some(url.into());
    }
    if url.starts_with("www.") {
        return Some(format!("https://{url}"));
    }
    if let Some(ctx) = ctx.filter(|ctx| ctx.working_tree) {
        let path = repo_path(&ctx.dir, url)?;
        return url::Url::from_file_path(ctx.repo.join(path))
            .ok()
            .map(Into::into);
    }
    let mut link = web_link(url, ctx)?;
    if let Some((_, frag)) = url.split_once('#') {
        link = format!("{link}#{frag}");
    }
    Some(link)
}

/// Markdown in the house style, with links resolved against the document.
fn markdown(state: &Entity<TextViewState>, ctx: Option<DocContext>) -> impl IntoElement {
    crate::chat::markdown(state, "git-row", vec![]).on_link_click(move |url, event, _, cx| {
        if (event.standard_click() || event.is_middle_click())
            && let Some(target) = link_url(url, ctx.as_ref())
        {
            cx.open_url(&target);
        }
    })
}

/// The spinner box shown while a diagram renders or a file loads.
fn loading(cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .h(px(120.))
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme.radius)
        .bg(theme.muted.opacity(0.4))
        .child(Spinner::new().color(theme.muted_foreground))
}

pub(crate) struct RichDoc {
    source: String,
    ctx: Option<DocContext>,
    items: Vec<Item>,
    dark: Option<bool>,
    cache: Entity<Lru>,
    /// Virtualizes the blocks when the document is read in full.
    list: Option<ListState>,
    width: Rc<std::cell::Cell<Pixels>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<Playing> for RichDoc {}

impl RichDoc {
    pub(crate) fn new(
        source: String,
        ctx: Option<DocContext>,
        cache: Entity<Lru>,
        full: bool,
    ) -> RichDoc {
        let mut items = vec![];
        flatten(blocks(&source, full), None, 0, &mut items);
        let list = full.then(|| ListState::new(items.len(), ListAlignment::Top, px(800.)));
        RichDoc {
            source,
            ctx,
            items,
            dark: None,
            cache,
            list,
            width: Rc::new(std::cell::Cell::new(px(0.))),
            _subs: vec![],
        }
    }

    pub(crate) fn source(&self) -> &str {
        &self.source
    }

    /// Stops any playing video, when its document leaves the screen.
    pub(crate) fn stop(&mut self, cx: &mut Context<Self>) {
        for item in &self.items {
            if let Some(View::Media(v)) = &item.view {
                v.update(cx, |v, cx| v.stop(cx));
            }
        }
    }

    fn start_diagram(&mut self, ix: usize, src: String, dark: bool, cx: &mut Context<Self>) {
        self.items[ix].view = Some(View::Loading);
        cx.spawn(async move |this, cx| {
            let out = cx
                .background_executor()
                .spawn({
                    let src = src.clone();
                    async move { mermaid::render(&src, dark) }
                })
                .await;
            this.update(cx, |s, cx| {
                // A theme change while rendering already started a newer one.
                if s.dark == Some(dark) {
                    s.items[ix].view = Some(match out {
                        Ok(d) => View::Diagram(d),
                        Err(e) => View::Unrendered(
                            e,
                            cx.new(|cx| TextViewState::markdown(&format!("```\n{src}```\n"), cx)),
                        ),
                    });
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn start_media(&mut self, ix: usize, url: String, alt: String, cx: &mut Context<Self>) {
        let ctx = self.ctx.clone();
        self.items[ix].view = Some(View::Loading);
        cx.spawn(async move |this, cx| {
            let out = cx
                .background_executor()
                .spawn(async move { resolve(&url, ctx.as_ref()) })
                .await;
            this.update(cx, |s, cx| {
                s.items[ix].view = Some(match out {
                    Ok(file) => {
                        let label = if alt.trim().is_empty() {
                            match file.kind {
                                Kind::Image => "Image".to_string(),
                                Kind::Video => "Video".to_string(),
                            }
                        } else {
                            alt
                        };
                        let (cache, width) = (s.cache.clone(), s.width.clone());
                        let view = cx.new(|_| MediaView::new(file, cache, width).only(1, label));
                        s._subs
                            .push(cx.subscribe(&view, |s, playing, _: &Playing, cx| {
                                for item in &s.items {
                                    if let Some(View::Media(v)) = &item.view
                                        && *v != playing
                                    {
                                        v.update(cx, |v, cx| v.stop(cx));
                                    }
                                }
                                cx.emit(Playing);
                            }));
                        View::Media(view)
                    }
                    Err(e) => View::Failed(e),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Whether a collapsed `Details` encloses item `ix`.
    fn hidden(&self, ix: usize) -> bool {
        let mut parent = self.items[ix].parent;
        while let Some(p) = parent {
            if !self.items[p].open {
                return true;
            }
            parent = self.items[p].parent;
        }
        false
    }

    /// Expands or collapses the `Details` item `ix`.
    fn toggle(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.items[ix].open = !self.items[ix].open;
        if let Some(list) = &self.list {
            let depth = self.items[ix].depth;
            let end = (ix + 1..self.items.len())
                .find(|&j| self.items[j].depth <= depth)
                .unwrap_or(self.items.len());
            list.remeasure_items(ix..end);
        }
        cx.notify();
    }

    /// Item `ix`'s Markdown view, made from `md` (with GitHub's inline links) on first use.
    fn text_state(&mut self, ix: usize, md: &str, cx: &mut Context<Self>) -> Entity<TextViewState> {
        if let Some(View::Text(state)) = &self.items[ix].view {
            return state.clone();
        }
        let web = self.ctx.as_ref().and_then(|c| c.web.as_deref());
        let md = enrich(&inline_images(md), web);
        let state = cx.new(|cx| TextViewState::markdown(&md, cx));
        self.items[ix].view = Some(View::Text(state.clone()));
        state
    }

    fn render_block(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        if self.hidden(ix) {
            return div().into_any_element();
        }
        let theme = cx.theme();
        let (muted, faint, radius) = (
            theme.muted_foreground,
            theme.muted.opacity(0.4),
            theme.radius,
        );
        let el = match self.items[ix].block.clone() {
            Block::Text(text) => {
                let state = self.text_state(ix, &text, cx);
                markdown(&state, self.ctx.clone()).into_any_element()
            }
            Block::Details { summary, .. } => {
                let open = self.items[ix].open;
                h_flex()
                    .id(("git-details", ix))
                    .test_support()
                    .gap_1()
                    .cursor_pointer()
                    .text_color(theme.foreground)
                    .child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .with_size(px(14.))
                        .text_color(muted),
                    )
                    .child(div().flex_1().min_w_0().child(summary))
                    .on_click(cx.listener(move |s, _, _, cx| {
                        cx.stop_propagation();
                        s.toggle(ix, cx)
                    }))
                    .into_any_element()
            }
            Block::Alert { kind, body } => {
                let (icon, color) = match kind {
                    "Note" => (IconName::Info, theme.link),
                    "Tip" => (IconName::Lightbulb, theme.success_foreground),
                    "Important" => (IconName::MessageSquareText, theme.primary),
                    "Warning" => (IconName::TriangleAlert, theme.warning_foreground),
                    _ => (IconName::CircleAlert, theme.danger_foreground),
                };
                let state = self.text_state(ix, &body, cx);
                v_flex()
                    .id(("git-alert", ix))
                    .test_support()
                    .gap_1()
                    .pl_3()
                    .border_l_2()
                    .border_color(color)
                    .child(
                        h_flex()
                            .gap_1p5()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(color)
                            .child(Icon::new(icon).with_size(px(14.)).text_color(color))
                            .child(kind),
                    )
                    .when(!body.trim().is_empty(), |d| {
                        d.child(markdown(&state, self.ctx.clone()))
                    })
                    .into_any_element()
            }
            Block::Suggestion(code) => {
                let theme = cx.theme();
                let lines = v_flex()
                    .py_1()
                    .bg(theme.muted)
                    .font_family(theme.mono_font_family.clone())
                    .text_size(theme.mono_font_size)
                    .children(code.lines().map(|line| {
                        h_flex()
                            .px_2()
                            .items_start()
                            .child(
                                div()
                                    .w(px(16.))
                                    .flex_none()
                                    .text_color(theme.success_foreground)
                                    .child("+"),
                            )
                            .child(div().flex_1().min_w_0().child(line.replace('\t', "    ")))
                    }));
                v_flex()
                    .id(("git-suggestion", ix))
                    .test_support()
                    .rounded(radius)
                    .border_1()
                    .border_color(theme.border)
                    .overflow_hidden()
                    .child(
                        h_flex()
                            .px_2()
                            .py_1()
                            .gap_1()
                            .text_xs()
                            .text_color(muted)
                            .child(Icon::new(IconName::FileDiff).with_size(px(12.)))
                            .child("Suggested change"),
                    )
                    .map(|d| {
                        if code.is_empty() {
                            d.child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .bg(theme.muted)
                                    .text_sm()
                                    .text_color(muted)
                                    .child("Removes the commented lines"),
                            )
                        } else {
                            d.child(lines)
                        }
                    })
                    .into_any_element()
            }
            Block::Mermaid(src) => match &self.items[ix].view {
                Some(View::Diagram(d)) => div()
                    .id(("git-mermaid", ix))
                    .test_support()
                    .relative()
                    .w_full()
                    .h(px(d.size.1.min(900.)))
                    .child(
                        img(d.png.clone())
                            .image_cache(&self.cache)
                            .absolute()
                            .inset_0()
                            .size_full()
                            .object_fit(ObjectFit::ScaleDown),
                    )
                    .into_any_element(),
                Some(View::Unrendered(e, source)) => v_flex()
                    .id(("git-mermaid-error", ix))
                    .test_support()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_1()
                            .text_xs()
                            .text_color(muted)
                            .child(Icon::new(IconName::TriangleAlert).with_size(px(12.)))
                            .child(format!("Mermaid diagram not rendered: {e}")),
                    )
                    .child(crate::chat::markdown(source, "git-row", vec![]))
                    .into_any_element(),
                Some(_) => loading(cx).into_any_element(),
                None => {
                    self.start_diagram(ix, src, cx.theme().is_dark(), cx);
                    loading(cx).into_any_element()
                }
            },
            Block::Media { url, alt } => {
                let link = web_link(&url, self.ctx.as_ref());
                let caption = h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(muted)
                    .child(div().flex_1().min_w_0().truncate().child(url.clone()))
                    .when_some(link, |d, link| {
                        d.child(
                            Button::new(("git-media-open", ix))
                                .label("Open in browser")
                                .icon(IconName::ExternalLink)
                                .ghost()
                                .xsmall()
                                .on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    cx.open_url(&link)
                                }),
                        )
                    });
                let body = match &self.items[ix].view {
                    Some(View::Media(view)) => view.clone().into_any_element(),
                    Some(View::Failed(e)) => div()
                        .id(("git-media-error", ix))
                        .test_support()
                        .w_full()
                        .p_3()
                        .rounded(radius)
                        .bg(faint)
                        .text_sm()
                        .text_color(muted)
                        .child(format!(
                            "Can't show {}: {e}",
                            if alt.trim().is_empty() {
                                "this file"
                            } else {
                                alt.as_str()
                            }
                        ))
                        .into_any_element(),
                    Some(_) => loading(cx).into_any_element(),
                    None => {
                        self.start_media(ix, url, alt, cx);
                        loading(cx).into_any_element()
                    }
                };
                v_flex()
                    .id(("git-media", ix))
                    .test_support()
                    .gap_1()
                    .child(body)
                    .child(caption)
                    .into_any_element()
            }
        };
        // A details body indents under its summary.
        div()
            .w_full()
            .min_w_0()
            .pb_2()
            .pl(px(16. * self.items[ix].depth as f32))
            .child(el)
            .into_any_element()
    }

    #[cfg(test)]
    pub(crate) fn rendered_len(&self, cx: &App) -> usize {
        self.items
            .iter()
            .filter_map(|i| match &i.view {
                Some(View::Text(t) | View::Unrendered(_, t)) => Some(t),
                _ => None,
            })
            .map(|t| t.read(cx).rendered_text().as_str().len())
            .sum()
    }

    #[cfg(test)]
    pub(crate) fn diagram_state(&self, ix: usize) -> Option<Result<PathBuf, String>> {
        match self.items[ix].view.as_ref()? {
            View::Diagram(d) => Some(Ok(d.png.clone())),
            View::Unrendered(e, _) => Some(Err(e.clone())),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn media_view(&self, ix: usize) -> Option<Entity<MediaView>> {
        match self.items.get(ix)?.view.as_ref()? {
            View::Media(v) => Some(v.clone()),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn media_error(&self, ix: usize) -> Option<String> {
        match self.items.get(ix)?.view.as_ref()? {
            View::Failed(e) => Some(e.clone()),
            _ => None,
        }
    }
}

impl Render for RichDoc {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dark = cx.theme().is_dark();
        if self.dark != Some(dark) {
            // Only diagrams follow the theme (text is styled when drawn); the cache keeps both.
            self.dark = Some(dark);
            for item in &mut self.items {
                if matches!(item.block, Block::Mermaid(_)) {
                    item.view = None;
                }
            }
        }
        match self.list.clone() {
            Some(list) => list_view(list, cx).into_any_element(),
            None => v_flex()
                .w_full()
                .min_w_0()
                .children((0..self.items.len()).map(|i| self.render_block(i, cx)))
                .into_any_element(),
        }
    }
}

fn list_view(state: ListState, cx: &mut Context<RichDoc>) -> impl IntoElement {
    list(
        state,
        cx.processor(|s, i, _, cx| {
            div()
                .w_full()
                .px_4()
                .child(
                    div()
                        .w_full()
                        .max_w(px(680.))
                        .mx_auto()
                        .child(s.render_block(i, cx)),
                )
                .into_any_element()
        }),
    )
    .size_full()
    .py_2()
}

impl RichDoc {
    pub(crate) fn list(&self) -> Option<&ListState> {
        self.list.as_ref()
    }

    pub(crate) fn scroll_by(&self, delta: Pixels) {
        if let Some(l) = &self.list {
            l.scroll_by(delta);
        }
    }

    pub(crate) fn scroll_to_start(&self) {
        if let Some(l) = &self.list {
            l.scroll_to(ListOffset::default());
        }
    }

    pub(crate) fn scroll_to_end(&self) {
        if let Some(l) = &self.list {
            l.scroll_to_end();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media(url: &str, alt: &str) -> Block {
        Block::Media {
            url: url.into(),
            alt: alt.into(),
        }
    }

    #[::core::prelude::v1::test]
    fn splits_fences_media_and_long_text() {
        let src = "Intro\n\n```mermaid\nflowchart TD\n A-->B\n```\n\n![shot](docs/a.png)\n\
                   <img width=\"300\" alt=\"demo\" src=\"https://github.com/user-attachments/assets/abc\">\n\
                   https://github.com/user-attachments/assets/def\n\
                   inline ![x](y.png) stays\n\
                   ```rust\n![not](media.png)\n```\n\
                   ![d](data:image/png;base64,AAAA)\n";
        let b = blocks(src, true);
        assert_eq!(b[0], Block::Text("Intro\n\n".into()));
        assert_eq!(b[1], Block::Mermaid("flowchart TD\n A-->B\n".into()));
        assert_eq!(b[2], media("docs/a.png", "shot"));
        assert_eq!(
            b[3],
            media("https://github.com/user-attachments/assets/abc", "demo")
        );
        assert_eq!(
            b[4],
            media("https://github.com/user-attachments/assets/def", "")
        );
        let Block::Text(rest) = &b[5] else {
            panic!("{b:?}")
        };
        assert!(rest.contains("inline ![x](y.png)") && rest.contains("![not](media.png)"));
        assert!(rest.contains("data:image"));
        assert_eq!(b.len(), 6);

        // Multibyte text never splits a character.
        for line in ["Hi 👋\n", "日本語で説明\n", "你好\n", "é<img\n"] {
            assert_eq!(blocks(line, true), [Block::Text(line.into())]);
        }
        assert_eq!(
            blocks(
                "[demo](https://github.com/user-attachments/assets/x)\n",
                true
            ),
            [media(
                "https://github.com/user-attachments/assets/x",
                "demo"
            )]
        );
        assert!(matches!(
            &blocks("[docs](https://x.test/a)\n", true)[0],
            Block::Text(_)
        ));

        let unclosed = blocks("a\n```mermaid\nflowchart TD\n", true);
        assert_eq!(
            unclosed.len(),
            1,
            "an unclosed fence stays source: {unclosed:?}"
        );

        let long = "word ".repeat(100) + "\n\n";
        let doc = long.repeat(50);
        let parts = blocks(&doc, true);
        assert!(parts.len() > 5, "long documents become several blocks");
        assert_eq!(blocks(&doc, false).len(), 1, "previews are not chunked");
        let joined: String = parts
            .iter()
            .map(|b| match b {
                Block::Text(t) => t.as_str(),
                _ => "",
            })
            .collect();
        assert_eq!(joined.trim_end(), doc.trim_end(), "nothing is lost");

        let table = format!("| a | b |\n|---|---|\n{}", "| cell | cell |\n".repeat(1000));
        let line = "wörd ".repeat(10_000) + "\n";
        let parts = blocks(&line, true);
        assert!(parts.len() > 5, "a newline-free paragraph is cut too");
        assert!(
            parts
                .iter()
                .all(|b| matches!(b, Block::Text(t) if t.len() <= HARD))
        );
        assert_eq!(blocks(&line, false).len(), 1);

        assert_eq!(
            blocks(&table, true).len(),
            1,
            "a long table stays one block"
        );
        let refs = format!(
            "See [one][1].\n\n{}\n\n[1]: https://x.test\n",
            "word ".repeat(600)
        );
        let parts = blocks(&refs, true);
        assert!(parts.len() > 1);
        assert!(matches!(&parts[0], Block::Text(t) if t.contains("[1]: https://x.test")));
    }

    fn text(b: &Block) -> &str {
        match b {
            Block::Text(t) => t,
            _ => panic!("not text: {b:?}"),
        }
    }

    #[::core::prelude::v1::test]
    fn html_comments_are_stripped_outside_code() {
        assert_eq!(strip_comments("a <!-- x --> b\n"), "a  b\n");
        assert_eq!(strip_comments("a\n<!-- one\ntwo -->\nb\n"), "a\n\nb\n");
        assert_eq!(strip_comments("a\n<!-- never closed\nb\n"), "a\n");
        let code = "```html\n<!-- kept -->\n```\nuse `<!--` and `` <!-- --> ``\n";
        assert_eq!(strip_comments(code), code);
        // A bot's state comment never reaches a block, however long, even when chunked.
        let bot = format!(
            "Summary\n\n<!-- internal state start -->\n\n<!-- {}\n\n-->\n\n<!-- internal state end -->\nTail\n",
            "QUJD".repeat(10_000)
        );
        let parts = blocks(&bot, true);
        assert!(parts.iter().all(|b| !text(b).contains("QUJD")), "{parts:?}");
        assert!(text(&parts[0]).contains("Summary") && text(&parts[0]).contains("Tail"));
    }

    #[::core::prelude::v1::test]
    fn details_fold_and_nest() {
        let src = "Before\n<details>\n<summary><b>Walk</b> &amp; through</summary>\n\nOuter\n\
                   <details open><summary>Inner</summary>\n\n```\n</details>\n```\nDeep\n</details>\n\
                   After outer\n</details> trailing\nEnd\n";
        let b = blocks(src, true);
        assert_eq!(text(&b[0]), "Before\n");
        let Block::Details {
            summary,
            open,
            body,
        } = &b[1]
        else {
            panic!("{b:?}")
        };
        assert_eq!((summary.as_str(), *open), ("Walk & through", false));
        assert!(text(&body[0]).contains("Outer"));
        let Block::Details {
            summary,
            open,
            body: inner,
        } = &body[1]
        else {
            panic!("{body:?}")
        };
        assert_eq!((summary.as_str(), *open), ("Inner", true));
        assert!(
            text(&inner[0]).contains("```\n</details>\n```") && text(&inner[0]).contains("Deep")
        );
        assert!(text(&body[2]).contains("After outer"));
        assert_eq!(text(&b[2]), "trailing\nEnd\n");
        assert_eq!(b.len(), 3);

        // Blank lines inside don't split it; no summary reads "Details"; unclosed runs on.
        let long = format!("<details>\n\n{}</details>\n", "word ".repeat(600) + "\n\n");
        let b = blocks(&long, true);
        assert!(matches!(&b[0], Block::Details { summary, .. } if summary == "Details"));
        assert_eq!(b.len(), 1);
        let b = blocks("<details><summary>S</summary>\nrest\n", true);
        assert!(matches!(&b[..], [Block::Details { body, .. }] if text(&body[0]) == "\nrest\n"));
        assert!(matches!(&blocks("<detailsx>\n", true)[0], Block::Text(_)));
        assert!(matches!(
            &blocks("```\n<details>\n```\n", true)[0],
            Block::Text(_)
        ));

        // The view flattens the body after its header and keys open state by item.
        let mut items = vec![];
        flatten(blocks(src, true), None, 0, &mut items);
        let shape: Vec<_> = items.iter().map(|i| (i.parent, i.depth, i.open)).collect();
        assert_eq!(
            shape,
            [
                (None, 0, false),
                (None, 0, false),
                (Some(1), 1, false),
                (Some(1), 1, true),
                (Some(3), 2, false),
                (Some(1), 1, false),
                (None, 0, false),
            ]
        );
    }

    #[::core::prelude::v1::test]
    fn alerts() {
        let b = blocks("> [!note]\n> Read this.\n>\n> More.\n\nAfter\n", true);
        assert_eq!(
            b[0],
            Block::Alert {
                kind: "Note",
                body: "Read this.\n\nMore.\n".into()
            }
        );
        assert_eq!(text(&b[1]), "\nAfter\n");
        // An unprefixed line continues the paragraph until a blank line or a new block.
        let b = blocks(
            "> [!NOTE]\n> long note that\nwraps here\n> and more\n\nAfter\n",
            true,
        );
        assert!(
            matches!(&b[0], Block::Alert { body, .. } if body == "long note that\nwraps here\nand more\n")
        );
        for (src, rest) in [
            ("> [!TIP]\n> a\n- list\n", "- list\n"),
            ("> [!TIP]\n> a\n# Head\n", "# Head\n"),
            ("> [!TIP]\n> a\n```\nx\n```\n", "```\nx\n```\n"),
            ("> [!TIP]\n>\nnot lazy\n", "not lazy\n"),
            ("> [!TIP]\n> a\n---\n", "---\n"),
            ("> [!TIP]\n> a\n_ _ _\n", "_ _ _\n"),
            ("> [!TIP]\n> a\n===\n", "===\n"),
            ("> [!TIP]\n> a\n<div>\nx\n", "<div>\nx\n"),
        ] {
            let b = blocks(src, true);
            assert!(matches!(&b[0], Block::Alert { .. }), "{src}");
            assert_eq!(text(&b[1]), rest, "{src}");
        }
        assert!(matches!(
            &blocks("> [!WARNING]\n", true)[0],
            Block::Alert {
                kind: "Warning",
                ..
            }
        ));
        for plain in [
            "> [!NOPE]\n> x\n",
            "> [!NOTE] inline\n",
            "> quote\n> [!TIP]\n",
        ] {
            assert!(
                blocks(plain, true)
                    .iter()
                    .all(|b| matches!(b, Block::Text(_))),
                "{plain}"
            );
        }
    }

    #[::core::prelude::v1::test]
    fn html_and_quoted_code_stay_unlinked() {
        let web = Some("https://github.com/o/r");
        let e = |s: &str| enrich(s, web);
        let link = |n: u32| format!("[#{n}](https://github.com/o/r/issues/{n})");
        // HTML blocks render as HTML, so Markdown links would show literally.
        for same in [
            "<table><tr><td>Fixes #42 by @alice abc1234f</td></tr></table>\n",
            "<div align=\"center\">\n#1 @me\n",
            "<pre>\n#1\n\n#2\n</pre>\n",
            "<!DOCTYPE html> #1\n",
            "<span class=\"x\">\n#1\n",
            "> ```\n> #1 @me\n> ```\n",
            "- ```\n  #1\n  ```\n",
            "<code>#1</code> <kbd>@me</kbd> <a href=\"x\">#3 :tada:</a> <PRE>#4</PRE>\n",
        ] {
            assert_eq!(e(same), same);
        }
        assert_eq!(
            e("<div>\n#1\n\n#2\n"),
            format!("<div>\n#1\n\n{}\n", link(2))
        );
        assert_eq!(
            e("<pre>x</pre>\n#2\n"),
            format!("<pre>x</pre>\n{}\n", link(2))
        );
        assert_eq!(
            e("<abbr>#2</abbr>\n"),
            format!("<abbr>{}</abbr>\n", link(2))
        );
        // A lone inline tag can't interrupt a paragraph, so it isn't an HTML block there.
        assert_eq!(
            e("text\n<span>\n#2\n"),
            format!("text\n<span>\n{}\n", link(2))
        );

        // Emoji in link text, but not in the destination or as nested links.
        assert_eq!(
            enrich("[:rocket: Deploy](https://x.test/:tada:)\n", None),
            "[🚀 Deploy](https://x.test/:tada:)\n"
        );
        assert_eq!(
            e("[#1 :tada:][ref] #2\n"),
            format!("[#1 🎉][ref] {}\n", link(2))
        );
    }

    #[::core::prelude::v1::test]
    fn comments_in_quoted_and_listed_code_are_kept() {
        for code in [
            "> ```html\n> <!--\n> ```\n\nImportant review feedback\n",
            "- ```\n  <!-- x\n  ```\n\nStill here\n",
            "1. ```html\n   <!-- y\n   ```\n",
        ] {
            assert_eq!(strip_comments(code), code);
        }
        assert_eq!(
            strip_comments("> ```\n> x\n> ```\n<!-- c -->ok\n"),
            "> ```\n> x\n> ```\nok\n"
        );
    }

    #[::core::prelude::v1::test]
    fn references_reach_alerts_and_details() {
        let src = "> [!NOTE]\n> See [docs][1].\n\n<details>\n\nAlso [docs][1].\n</details>\n\n\
                   [1]: https://x.test\n";
        let b = blocks(src, true);
        assert!(
            matches!(&b[0], Block::Alert { body, .. } if body.ends_with("\n\n[1]: https://x.test\n"))
        );
        let Block::Details { body, .. } = &b[1] else {
            panic!("{b:?}")
        };
        assert!(
            text(&body[0]).contains("Also [docs][1].")
                && text(&body[0]).ends_with("[1]: https://x.test\n")
        );
        // Nothing is appended to an empty alert.
        assert!(
            matches!(&blocks("> [!NOTE]\n\n[1]: https://x.test\n", true)[0], Block::Alert { body, .. } if body.is_empty())
        );
    }

    #[::core::prelude::v1::test]
    fn blocks_get_only_the_references_they_use() {
        let src = format!(
            "A uses [one].\n\n{}\n\n<details>\n\nB uses [Two\n Words][] and [x][THREE].\n\
             </details>\n\n[one]: https://u1\n[two words]: https://u2\n[three]: https://u3\n\
             [unused]: https://u4\n",
            "word ".repeat(600)
        );
        let b = blocks(&src, true);
        assert!(text(&b[0]).starts_with("A uses [one]."));
        assert!(text(&b[0]).ends_with("word\n\n[one]: https://u1\n"));
        let body = b
            .iter()
            .find_map(|b| match b {
                Block::Details { body, .. } => Some(text(&body[0])),
                _ => None,
            })
            .unwrap();
        assert!(
            body.ends_with("\n\n[two words]: https://u2\n[three]: https://u3\n"),
            "{body:?}"
        );
        assert!(!body.contains("u1") && !body.contains("u4"));
        // The block holding the definitions gets no second copy.
        let last = text(b.last().unwrap());
        assert_eq!(last.matches("https://u2").count(), 1, "{last:?}");

        // Thousands of definitions grow the output linearly, not per block.
        let n = 2000;
        let mut doc: String = (0..n)
            .map(|i| format!("Paragraph {i} cites [r{i}] {}\n\n", "word ".repeat(20)))
            .collect();
        doc.extend((0..n).map(|i| format!("[r{i}]: https://x.test/{i}\n")));
        let parts = blocks(&doc, true);
        assert!(parts.len() > 50);
        let total: usize = parts.iter().map(|b| text(b).len()).sum();
        assert!(total < 3 * doc.len(), "{total} vs {}", doc.len());
    }

    #[::core::prelude::v1::test]
    fn fences_in_lists_stay_code() {
        let src = "- example:\n\n  ```md\n  ![sample](https://example.test/a.png)\n  <details>\n\
                   \x20 > [!NOTE]\n  > x\n  ```\n![real](https://example.test/b.png)\n";
        let b = blocks(src, true);
        assert!(text(&b[0]).contains("  ![sample](https://example.test/a.png)\n  <details>"));
        assert!(text(&b[0]).contains("  > [!NOTE]\n  > x\n  ```\n"));
        assert_eq!(b[1], media("https://example.test/b.png", "real"));
        assert_eq!(b.len(), 2, "{b:?}");
    }

    #[::core::prelude::v1::test]
    fn comments_in_indented_code_are_kept() {
        for same in [
            "Example:\n\n    <!-- marker -->\n",
            "\t<!-- x\n",
            "a\n\n    one\n\n    <!-- two -->\n",
        ] {
            assert_eq!(strip_comments(same), same);
        }
        assert_eq!(
            text(&blocks("Example:\n\n    <!-- marker -->\n", true)[0]),
            "Example:\n\n    <!-- marker -->\n"
        );
        // A paragraph or list continuation is not code.
        assert_eq!(strip_comments("text\n    <!-- c -->\n"), "text\n    \n");
        assert_eq!(
            strip_comments("- item\n\n    <!-- c -->\n"),
            "- item\n\n    \n"
        );
    }

    #[::core::prelude::v1::test]
    fn review_round_four() {
        // 1. Only a line-opening `<!--` hides to the end; inline ones close in their paragraph.
        for same in [
            "Use <!-- to start a comment.\n\nMore\n",
            "a <!-- x\n\ny --> b\n",
            "a <!-- x --><!-- y\n\nb\n",
        ] {
            let want = same.replace("<!-- x -->", "");
            assert_eq!(strip_comments(same), want.as_str());
        }
        assert_eq!(strip_comments("a <!-- x\ny --> b\n"), "a  b\n");
        assert_eq!(strip_comments("a\n   <!-- open\nb\n"), "a\n   ");

        // 2. Details tags in inline code or lenient fences don't count.
        let b = blocks(
            "<details><summary>S</summary>\nUse `</details>` here\n> ```\n> </details>\n> ```\n\
             </details>\nafter\n",
            true,
        );
        let Block::Details { body, .. } = &b[0] else {
            panic!("{b:?}")
        };
        assert!(text(&body[0]).contains("`</details>` here\n> ```\n> </details>\n> ```"));
        assert_eq!(text(&b[1]), "after\n");
        assert_eq!(b.len(), 2);

        // 3. A preview is never blank for a non-blank body.
        let (p, cut) = preview(&format!("\n\n{}", "word ".repeat(100)), 50);
        assert!(
            cut && p.trim().starts_with("word") && p.len() <= 50,
            "{p:?}"
        );
        let (p, cut) = preview(&format!("\n\n    {}", "x".repeat(100)), 10);
        assert!(cut && !p.trim().is_empty(), "{p:?}");
        assert_eq!(preview("abcdef ghi", 3), ("abc".into(), true));
        assert_eq!(preview("\n\n  abcdef ghi", 1), ("abcdef".into(), true));

        // 4. Indented code is left alone; list continuations and paragraph lines are not.
        let web = Some("https://github.com/o/r");
        let link = |n: u32| format!("[#{n}](https://github.com/o/r/issues/{n})");
        for same in ["para\n\n    #1 @me\n\n    #2\n", "\t#1\n", "x\n\n  \t#1\n"] {
            assert_eq!(enrich(same, web), same);
        }
        assert_eq!(
            enrich("para\n\n    #1\nafter #2\n", web),
            format!("para\n\n    #1\nafter {}\n", link(2))
        );
        assert_eq!(
            enrich("- item\n\n    continued #1\n", web),
            format!("- item\n\n    continued {}\n", link(1))
        );
        assert_eq!(
            enrich("text\n    #1\n", web),
            format!("text\n    {}\n", link(1))
        );
    }

    #[::core::prelude::v1::test]
    fn inline_images_become_alt_text() {
        for (src, want) in [
            (
                "Gates Passed <img src=\"a.svg\" alt=\"3\" width=\"16\"> Quality\n",
                "Gates Passed 3 Quality\n",
            ),
            (
                "<img src=\"m.png\" alt=\"Minor\"> Functional\n",
                "Minor Functional\n",
            ),
            (
                "- <IMG SRC=x.svg> bullet <img alt='' src=y>\n",
                "-  bullet \n",
            ),
            (
                "<picture><source media=\"dark\" srcset=\"d.png\"><img src=\"l.png\" alt=\"Logo\"></picture> text\n",
                "Logo text\n",
            ),
            (
                "<picture>\n  <source srcset=\"d.png\">\n  <img src=\"l.png\" alt=\"Logo\">\n</picture>\n",
                "\n  \n  Logo\n\n",
            ),
            (
                "<p align=\"center\"><img src=x alt=\"Badge\"></p>\n",
                "<p align=\"center\">Badge</p>\n",
            ),
        ] {
            assert_eq!(inline_images(src), want, "{src}");
        }
        for same in [
            "`<img src=x alt=y>` and <imgx> <sourcery>\n",
            "```html\n<img src=x alt=y>\n```\n",
            "no images\n",
        ] {
            assert_eq!(inline_images(same), same);
        }
        // A line of only images is still a media block.
        assert_eq!(
            blocks("<img src=\"https://x.test/a.png\" alt=\"A\">\n", true),
            [Block::Media {
                url: "https://x.test/a.png".into(),
                alt: "A".into()
            }]
        );
    }

    #[::core::prelude::v1::test]
    fn suggestions() {
        let b = blocks("Try:\n```suggestion\nlet x = 1;\n```\n", true);
        assert_eq!(b[1], Block::Suggestion("let x = 1;\n".into()));
        assert_eq!(
            blocks("```suggestion\n```\n", true),
            [Block::Suggestion(String::new())]
        );
        // A blank-line replacement is a line, not a deletion (the renderer tests `is_empty`).
        assert_eq!(
            blocks("```suggestion\n  \n```\n", true),
            [Block::Suggestion("  \n".into())]
        );
    }

    #[::core::prelude::v1::test]
    fn summaries_get_emoji_and_details_end_lazy_alerts() {
        let b = blocks(
            "<details><summary>:warning: 3 flaky tests</summary>\nx\n</details>\n",
            true,
        );
        assert!(matches!(&b[0], Block::Details { summary, .. } if summary == "⚠️ 3 flaky tests"));
        let b = blocks(
            "> [!TIP]\n> a\n<details><summary>S</summary>\nb\n</details>\n",
            true,
        );
        assert!(matches!(&b[0], Block::Alert { body, .. } if body == "a\n"));
        assert!(matches!(&b[1], Block::Details { .. }));
        // Ordinary text with dashes still continues lazily.
        let b = blocks("> [!TIP]\n> a\nb -- c\n", true);
        assert!(matches!(&b[0], Block::Alert { body, .. } if body == "a\nb -- c\n"));
    }

    #[::core::prelude::v1::test]
    fn inline_links_and_emoji() {
        let web = Some("https://github.com/o/r");
        let e = |s: &str| enrich(s, web);
        assert_eq!(
            e("cc @kasper, fixes #12 and o2/r-2#3 in abc1234f\n"),
            "cc [@kasper](https://github.com/kasper), fixes [#12](https://github.com/o/r/issues/12) \
             and [o2/r-2#3](https://github.com/o2/r-2/issues/3) in \
             [`abc1234`](https://github.com/o/r/commit/abc1234f)\n"
        );
        for same in [
            "mail me@example.com\n",
            "see https://x.test/a#12 and www.x.test/@me\n",
            "`#12 @me abc1234f` and ``x `@me` y``\n",
            "[#12 @me](https://x.test) [ref][1] ![@me](a.png)\n",
            "<a href=\"#12\">x</a>\n",
            "# Heading\n",
            "&#123; issue#12 @team/x 1234567 deadbeef\n",
            "[1]: https://x.test/#12\n",
            "```\n@me #12\n```\n",
        ] {
            assert_eq!(e(same), same);
        }
        assert_eq!(
            e("- [ ] #4\n# Fix #5\n"),
            "- [ ] [#4](https://github.com/o/r/issues/4)\n# Fix [#5](https://github.com/o/r/issues/5)\n"
        );
        // Without a repository only emoji change.
        assert_eq!(
            enrich("@me #1 :tada: :nope: 10:30:00 :+1:\n", None),
            "@me #1 🎉 :nope: 10:30:00 👍\n"
        );
        assert_eq!(enrich("`:tada:`\n", None), "`:tada:`\n");
        assert_eq!(
            enrich("日本 @me\n", web),
            "日本 [@me](https://github.com/me)\n"
        );
    }

    #[::core::prelude::v1::test]
    fn previews_cut_at_blocks() {
        assert_eq!(preview("short <!-- x -->\n", 100), ("short".into(), false));
        let src = format!("{}\n\n{}\n", "a".repeat(60), "b".repeat(60));
        assert_eq!(preview(&src, 100), ("a".repeat(60), true));
        // Never inside a fence: the cut lands before it.
        let src = format!(
            "Intro\n\n```rust\n{}```\nafter\n",
            "let x = 1;\n".repeat(20)
        );
        let (p, cut) = preview(&src, 120);
        assert!(cut);
        assert_eq!(p, "Intro");
        // A fence that opens the body is closed after its last whole line.
        let src = format!("```rust\n{}```\n", "let x = 1;\n".repeat(20));
        let (p, cut) = preview(&src, 50);
        assert!(
            cut && p.ends_with("let x = 1;\n```") && p.len() <= 53,
            "{p:?}"
        );
        assert_eq!(p.matches("```").count(), 2);
        // One long line is cut at a space, on a character boundary.
        let (p, cut) = preview(&"wörd ".repeat(100), 50);
        assert!(cut && p.len() <= 50 && p.ends_with("wörd"));
    }

    #[::core::prelude::v1::test]
    fn relative_paths_stay_in_the_repository() {
        assert_eq!(
            repo_path("docs", "img/a%20b.png").as_deref(),
            Some("docs/img/a b.png")
        );
        assert_eq!(
            repo_path("docs", "../a.png?raw=1").as_deref(),
            Some("a.png")
        );
        assert_eq!(repo_path("docs", "/root.png").as_deref(), Some("root.png"));
        assert_eq!(repo_path("", "../../etc/passwd"), None);
        let ctx = DocContext {
            repo: "/r".into(),
            rev: Some("abc".into()),
            working_tree: false,
            dir: "docs".into(),
            web: Some("https://github.com/o/r".into()),
        };
        assert_eq!(
            web_link("a.png", Some(&ctx)).as_deref(),
            Some("https://github.com/o/r/blob/abc/docs/a.png")
        );
        assert_eq!(
            web_link("a%23b%20c.png", Some(&ctx)).as_deref(),
            Some("https://github.com/o/r/blob/abc/docs/a%23b%20c.png")
        );
        assert_eq!(
            link_url("../README.md#usage", Some(&ctx)).as_deref(),
            Some("https://github.com/o/r/blob/abc/README.md#usage")
        );
        assert_eq!(
            link_url("https://x.test/a", Some(&ctx)).as_deref(),
            Some("https://x.test/a")
        );
        assert_eq!(link_url("#usage", Some(&ctx)), None);
        assert_eq!(link_url("docs/a.md", None), None);
        let working = DocContext {
            working_tree: true,
            ..ctx.clone()
        };
        assert_eq!(
            link_url("a%20b.md", Some(&working)).as_deref(),
            Some("file:///r/docs/a%20b.md")
        );
        assert_eq!(link_url("../../outside.md", Some(&working)), None);
        assert_eq!(
            attr("<img alt='x y' src=a.png>", "src").as_deref(),
            Some("a.png")
        );
        assert_eq!(
            attr("<img data-src=\"no\" src=\"yes\">", "src").as_deref(),
            Some("yes")
        );
    }

    #[::core::prelude::v1::test]
    fn markdown_assets_use_the_selected_revision_or_working_tree() {
        use crate::git::git;
        use std::fs;
        let temp = std::env::temp_dir().join(format!(
            "shark-markdown-assets-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let repo = temp.join("repo");
        fs::create_dir_all(repo.join("docs")).unwrap();
        git(&repo, &["init", "-q"]).unwrap();
        fs::write(repo.join("docs/image.png"), b"before").unwrap();
        git(&repo, &["add", "."]).unwrap();
        git(
            &repo,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-qm",
                "base",
            ],
        )
        .unwrap();
        let mut ctx = DocContext {
            repo: repo.clone(),
            rev: Some(git(&repo, &["rev-parse", "HEAD"]).unwrap()),
            working_tree: false,
            dir: "docs".into(),
            web: None,
        };
        fs::write(repo.join("docs/image.png"), b"after").unwrap();
        let before = resolve("image.png", Some(&ctx)).unwrap().new.unwrap();
        assert_eq!(before.source, Source::Blob);
        assert_eq!(
            crate::git::history::blob(&repo, &before.sha).unwrap(),
            b"before"
        );
        ctx.working_tree = true;
        let after = resolve("image.png", Some(&ctx)).unwrap().new.unwrap();
        assert!(matches!(after.source, Source::Working(_)));
        assert_ne!(after.sha, before.sha);
        fs::write(temp.join("outside.png"), b"outside").unwrap();
        std::os::unix::fs::symlink(temp.join("outside.png"), repo.join("docs/escape.png")).unwrap();
        assert!(resolve("escape.png", Some(&ctx)).is_err());
        assert!(resolve("../../outside.png", Some(&ctx)).is_err());
        fs::remove_dir_all(temp).unwrap();
    }
}
