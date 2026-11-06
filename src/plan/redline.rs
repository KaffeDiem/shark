//! The pure markdown redline: a plan version with what changed since the one before marked inline.
use similar::{Algorithm, DiffTag, capture_diff_slices};
use std::ops::Range;

/// A plan version rendered with what changed since the previous one marked inline.
#[derive(Debug, Default, PartialEq)]
pub struct Redline {
    pub md: String,
    /// Source ranges in `md` of each change, in order, for jumping between them.
    pub hunks: Vec<Range<usize>>,
    /// Words added and removed.
    pub added: usize,
    pub removed: usize,
}

/// `new` as markdown, with words added since `old` on an `ins` background and removed ones
/// struck through on `del`. Lines are aligned first, then words within a changed line, so the
/// result keeps its headings and lists. A changed code block becomes a `diff` block.
pub fn redline(old: &str, new: &str, ins: &str, del: &str) -> Redline {
    let (old, new) = (with_newline(old), with_newline(new));
    let (a, b) = (units(&old), units(&new));
    let mut r = Redline::default();
    let m = Marks { ins, del };
    for op in capture_diff_slices(Algorithm::Patience, &a, &b) {
        let (tag, ra, rb) = op.as_tag_tuple();
        let start = r.md.len();
        if tag == DiffTag::Equal {
            rb.for_each(|i| r.md.push_str(b[i]));
            continue;
        }
        let (olds, news) = (&a[ra], &b[rb]);
        for i in 0..olds.len().max(news.len()) {
            let unit = match (olds.get(i), news.get(i)) {
                (Some(o), Some(n)) => m.changed(o, n, &mut r),
                (Some(o), None) => m.removed(o, &mut r),
                (None, Some(n)) => m.added(n, &mut r),
                (None, None) => unreachable!(),
            };
            r.md.push_str(&unit);
        }
        // Blank lines coming or going aren't worth a stop.
        if olds.iter().chain(news).any(|u| !u.trim().is_empty()) {
            r.hunks.push(start..r.md.len());
        }
    }
    r
}

fn with_newline(s: &str) -> String {
    match s.ends_with('\n') {
        true => s.to_string(),
        false => format!("{s}\n"),
    }
}

/// Lines, except that a fenced code block is one unit: its lines can't carry marks. A fence
/// closes on a run of its own character at least as long as the opener's.
fn units(s: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut fence: Option<(usize, char, usize)> = None;
    let mut pos = 0;
    for line in s.split_inclusive('\n') {
        let t = line.trim();
        let run = |c: char| t.chars().take_while(|&x| x == c).count();
        match fence {
            None => match ['`', '~'].into_iter().find(|&c| run(c) >= 3) {
                Some(c) => fence = Some((pos, c, run(c))),
                None => out.push(line),
            },
            Some((start, c, n)) if run(c) >= n && run(c) == t.chars().count() => {
                out.push(&s[start..pos + line.len()]);
                fence = None;
            }
            Some(_) => {}
        }
        pos += line.len();
    }
    if let Some((start, ..)) = fence {
        out.push(&s[start..]);
    }
    out
}

fn is_fence(u: &str) -> bool {
    let t = u.trim_start();
    t.starts_with("```") || t.starts_with("~~~")
}

/// Bytes of a line's block syntax: indent, `> `, `## `, `- `, `1. `, `[ ] `. Marks go after it.
fn marker_len(line: &str) -> usize {
    let mut i = line.len() - line.trim_start().len();
    loop {
        let rest = &line[i..];
        if let Some(r) = rest.strip_prefix("> ") {
            i = line.len() - r.len();
            continue;
        }
        let hashes = rest.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hashes) && rest[hashes..].starts_with(' ') {
            return i + hashes + 1;
        }
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        let bullet = if ["- ", "* ", "+ "].iter().any(|b| rest.starts_with(b)) {
            2
        } else if (1..10).contains(&digits)
            && (rest[digits..].starts_with(". ") || rest[digits..].starts_with(") "))
        {
            digits + 2
        } else {
            return i;
        };
        i += bullet;
        let rest = &line[i..];
        if ["[ ] ", "[x] ", "[X] "].iter().any(|t| rest.starts_with(t)) {
            i += 4;
        }
        return i;
    }
}

/// Lines with nothing to mark: rules (`---`) and table separators.
fn is_rule(body: &str) -> bool {
    let t = body.trim();
    !t.is_empty() && t.chars().all(|c| "-*_=|: ".contains(c))
}

fn is_table(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

/// A table row's cells, without the outer pipes.
fn cells(line: &str) -> Vec<&str> {
    let t = line.trim().trim_start_matches('|');
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').collect()
}

/// Words, whitespace runs, and code spans and links whole: marks inside those would show
/// as text.
fn tokens(s: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        let c = rest.chars().next().unwrap();
        let len = if c == '`' {
            let ticks = rest.chars().take_while(|&c| c == '`').count();
            let fence = &rest[..ticks];
            rest[ticks..]
                .find(fence)
                .map_or(ticks, |end| ticks + end + ticks)
        } else if c == '[' {
            // [text](url), else just the bracket's word: the first ] must open the (url)
            match rest.find(']') {
                Some(m) if rest[m + 1..].starts_with('(') && !rest[..m].contains('\n') => {
                    rest[m..].find(')').map_or(word_len(rest), |e| m + e + 1)
                }
                _ => word_len(rest),
            }
        } else if c.is_whitespace() {
            rest.find(|c: char| !c.is_whitespace())
                .unwrap_or(rest.len())
        } else {
            word_len(rest)
        };
        out.push(&rest[..len]);
        i += len;
    }
    out
}

fn word_len(s: &str) -> usize {
    let first = s.chars().next().map_or(0, char::len_utf8);
    first
        + s[first..]
            .find(|c: char| c.is_whitespace() || c == '`' || c == '[')
            .unwrap_or(s.len() - first)
}

/// Adds `old`'s words to the removed total and `new`'s to the added one.
fn count(r: &mut Redline, old: &str, new: &str) {
    r.removed += words(&tokens(old));
    r.added += words(&tokens(new));
}

fn words(toks: &[&str]) -> usize {
    toks.iter().filter(|t| !t.trim().is_empty()).count()
}

/// Emphasis delimiters a mark could split from their partner, which would then show as text.
fn unsafe_token(t: &str) -> bool {
    !t.starts_with('`')
        && (t.contains('*') || t.contains("~~") || t.starts_with('_') || t.ends_with('_'))
}

struct Marks<'a> {
    ins: &'a str,
    del: &'a str,
}

impl Marks<'_> {
    /// Wraps the text, leaving its outer whitespace outside the tags.
    fn wrap(&self, s: &str, add: bool) -> String {
        let core = s.trim();
        if core.is_empty() {
            return s.to_string();
        }
        let lead = &s[..s.len() - s.trim_start().len()];
        let trail = &s[s.trim_end().len()..];
        match add {
            true => format!("{lead}<mark color=\"{}\">{core}</mark>{trail}", self.ins),
            false => format!(
                "{lead}<del><mark color=\"{}\">{core}</mark></del>{trail}",
                self.del
            ),
        }
    }

    /// A line or table row with all of its text marked.
    fn whole(&self, line: &str, add: bool) -> String {
        let (line, nl) = match line.strip_suffix('\n') {
            Some(l) => (l, "\n"),
            None => (line, ""),
        };
        if is_table(line) && !is_rule(line) {
            let cells: Vec<String> = cells(line).iter().map(|c| self.wrap(c, add)).collect();
            return format!("|{}|{nl}", cells.join("|"));
        }
        let at = marker_len(line);
        let (head, body) = line.split_at(at);
        if is_rule(body) {
            return format!("{line}{nl}");
        }
        format!("{head}{}{nl}", self.wrap(body, add))
    }

    fn added(&self, unit: &str, r: &mut Redline) -> String {
        r.added += words(&tokens(&unit[marker_len(unit)..]));
        match is_fence(unit) {
            true => fence_diff("", unit),
            false => self.whole(unit, true),
        }
    }

    fn removed(&self, unit: &str, r: &mut Redline) -> String {
        r.removed += words(&tokens(&unit[marker_len(unit)..]));
        match is_fence(unit) {
            true => fence_diff(unit, ""),
            false if unit.trim().is_empty() => unit.to_string(),
            false => self.whole(unit, false),
        }
    }

    fn changed(&self, old: &str, new: &str, r: &mut Redline) -> String {
        if is_fence(old) != is_fence(new) {
            // Code replaced by prose or the reverse: the old struck, the new under it.
            return format!("{}{}", self.removed(old, r), self.added(new, r));
        }
        if is_fence(old) {
            count(r, old, new);
            return fence_diff(old, new);
        }
        let (old, new) = (old.trim_end_matches('\n'), new.trim_end_matches('\n'));
        if is_table(old) && is_table(new) && !is_rule(new) {
            let (ca, cb) = (cells(old), cells(new));
            if ca.len() == cb.len() {
                let row: Vec<String> = (ca.iter().zip(&cb))
                    .map(|(a, b)| {
                        self.inline(a, b, r).unwrap_or_else(|| {
                            count(r, a, b);
                            let (a, b) = (self.wrap(a.trim(), false), self.wrap(b.trim(), true));
                            format!(" {a} {b} ")
                        })
                    })
                    .collect();
                return format!("|{}|\n", row.join("|"));
            }
        }
        let (ma, mb) = (marker_len(old), marker_len(new));
        if old[..ma].trim() != new[..mb].trim() {
            // A different kind of block: the old line struck, the new one under it.
            return format!("{}\n{}\n", self.removed(old, r), self.added(new, r));
        }
        let (a, b) = (&old[ma..], &new[mb..]);
        if let Some(body) = self.inline(a, b, r) {
            return format!("{}{body}\n", &new[..mb]);
        }
        // Too different to mark word by word: all of it struck, then all of it new.
        count(r, a, b);
        format!(
            "{}{} {}\n",
            &new[..mb],
            self.wrap(a, false),
            self.wrap(b, true)
        )
    }

    /// The words of `new` with changes from `old` marked, or `None` when they share too
    /// little for that to read well, or a mark would split emphasis.
    fn inline(&self, old: &str, new: &str, r: &mut Redline) -> Option<String> {
        let (ta, tb) = (tokens(old), tokens(new));
        let ops = capture_diff_slices(Algorithm::Patience, &ta, &tb);
        let same: usize = (ops.iter())
            .filter(|op| op.tag() == DiffTag::Equal)
            .map(|op| words(&tb[op.new_range()]))
            .sum();
        if same * 2 < words(&ta).max(words(&tb)) {
            return None;
        }
        let mut out = String::new();
        let (mut added, mut removed) = (0, 0);
        for op in &ops {
            let (tag, ra, rb) = op.as_tag_tuple();
            let (gone, came) = (&ta[ra], &tb[rb]);
            if tag != DiffTag::Equal && gone.iter().chain(came).any(|t| unsafe_token(t)) {
                return None;
            }
            match tag {
                DiffTag::Equal => out.push_str(&came.concat()),
                _ if words(gone) + words(came) == 0 => out.push_str(&came.concat()),
                _ => {
                    if words(gone) > 0 {
                        out.push_str(&self.wrap(&gone.concat(), false));
                        removed += words(gone);
                    }
                    if words(came) > 0 {
                        // A space between the struck and the new words.
                        if words(gone) > 0 && !came[0].trim().is_empty() {
                            out.push(' ');
                        }
                        out.push_str(&self.wrap(&came.concat(), true));
                        added += words(came);
                    }
                }
            }
        }
        r.added += added;
        r.removed += removed;
        Some(out)
    }
}

/// A code block as a `diff` block: `-` for old lines, `+` for new, ` ` for kept.
fn fence_diff(old: &str, new: &str) -> String {
    let inner = |u: &str| -> Vec<String> {
        let lines: Vec<&str> = u.lines().collect();
        match lines.len() {
            0 => vec![],
            // the opening fence, and the closing one when there is one
            n => lines[1..n - (n > 1 && is_fence(lines[n - 1])) as usize]
                .iter()
                .map(|l| l.to_string())
                .collect(),
        }
    };
    let (a, b) = (inner(old), inner(new));
    let mut out = String::from("```diff\n");
    for op in capture_diff_slices(Algorithm::Patience, &a, &b) {
        let (tag, ra, rb) = op.as_tag_tuple();
        if tag == DiffTag::Equal {
            rb.for_each(|i| out += &format!(" {}\n", b[i]));
            continue;
        }
        ra.for_each(|i| out += &format!("-{}\n", a[i]));
        rb.for_each(|i| out += &format!("+{}\n", b[i]));
    }
    out + "```\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rl(old: &str, new: &str) -> Redline {
        redline(old, new, "+", "-")
    }

    const INS: &str = "<mark color=\"+\">";
    const DEL: &str = "<del><mark color=\"-\">";

    #[test]
    fn marks_words_inside_a_changed_line() {
        let r = rl(
            "# Plan\n\n- Add a cache to the fetch\n- Ship it\n",
            "# Plan\n\n- Add an LRU cache to `fetch`\n- Ship it\n",
        );
        assert_eq!(
            r.md,
            format!(
                "# Plan\n\n- Add {DEL}a</mark></del> {INS}an LRU</mark> cache to {DEL}the fetch</mark></del> {INS}`fetch`</mark>\n- Ship it\n"
            )
        );
        assert_eq!((r.added, r.removed), (3, 3));
        assert_eq!(r.hunks.len(), 1);
    }

    #[test]
    fn whole_lines_keep_their_block_syntax() {
        let r = rl(
            "## Steps\n1. Read\n",
            "## Steps\n1. Read\n2. Write tests\n\n## Risks\n",
        );
        assert_eq!(
            r.md,
            format!("## Steps\n1. Read\n2. {INS}Write tests</mark>\n\n## {INS}Risks</mark>\n")
        );
        let r = rl("- [ ] one\n- [ ] two\n", "- [ ] one\n");
        assert_eq!(r.md, format!("- [ ] one\n- [ ] {DEL}two</mark></del>\n"));
        assert_eq!((r.added, r.removed), (0, 1));
    }

    #[test]
    fn unrelated_rewrites_strike_then_add() {
        let r = rl("Use Redis.\n", "Keep everything in memory instead.\n");
        assert_eq!(
            r.md,
            format!(
                "{DEL}Use Redis.</mark></del> {INS}Keep everything in memory instead.</mark>\n"
            )
        );
    }

    #[test]
    fn never_splits_emphasis() {
        let r = rl("**Step 1:** read\n", "**Step 2:** read\n");
        assert!(!r.md.contains(&format!("{DEL}1:**")), "{}", r.md);
        assert!(r.md.contains(&format!("{DEL}**Step 1:** read</mark></del>")));
    }

    #[test]
    fn code_blocks_become_diff_blocks() {
        let r = rl(
            "Run:\n```sh\nmake\nmake test\n```\n",
            "Run:\n```sh\nmake\ncargo test\n```\n",
        );
        assert_eq!(r.md, "Run:\n```diff\n make\n-make test\n+cargo test\n```\n");
        let r = rl("```\na\n```\n", "```\na\n```\n");
        assert_eq!(r.md, "```\na\n```\n");
        assert!(r.hunks.is_empty());
        let r = rl("```sh\nmake\n```\n", "Run make by hand.\n");
        assert_eq!(
            r.md,
            format!("```diff\n-make\n```\n{INS}Run make by hand.</mark>\n")
        );
    }

    #[test]
    fn a_fence_closes_only_on_a_run_as_long_as_its_opener() {
        let md = "a\n````md\n```sh\nmake\n```\n````\nb\n";
        assert_eq!(
            units(md),
            ["a\n", "````md\n```sh\nmake\n```\n````\n", "b\n"]
        );
        let md = "~~~\n```\n~~~\nb\n";
        assert_eq!(units(md), ["~~~\n```\n~~~\n", "b\n"]);
        // Unclosed: the rest is one unit.
        assert_eq!(units("a\n```\nb\n"), ["a\n", "```\nb\n"]);
    }

    #[test]
    fn a_link_needs_its_url_right_after_the_bracket() {
        assert_eq!(
            tokens("[WIP] fix [docs](http://x/y) now"),
            ["[WIP]", " ", "fix", " ", "[docs](http://x/y)", " ", "now"]
        );
        assert_eq!(tokens("[a b](u)"), ["[a b](u)"]);
    }

    #[test]
    fn table_cells_are_marked_in_place() {
        let r = rl(
            "| a | b |\n|---|---|\n| x | old |\n",
            "| a | b |\n|---|---|\n| x | new |\n",
        );
        assert_eq!(
            r.md,
            format!("| a | b |\n|---|---|\n| x | {DEL}old</mark></del> {INS}new</mark> |\n")
        );
    }

    #[test]
    fn hunks_point_at_each_change() {
        let old = "# A\n\none\n\ntwo\n\nthree\n";
        let new = "# A\n\none!\n\ntwo\n\nthree\n\nfour\n";
        let r = rl(old, new);
        assert_eq!(r.hunks.len(), 2);
        assert!(r.md[r.hunks[0].clone()].contains("one"));
        assert!(r.md[r.hunks[1].clone()].contains("four"));
        assert_eq!(rl(old, old).md, old);
    }

    #[test]
    fn totals_count_words_not_marks() {
        let mut r = Redline::default();
        count(&mut r, "one two", "three");
        count(&mut r, "`x`", "");
        assert_eq!((r.removed, r.added), (3, 1));
    }
}
