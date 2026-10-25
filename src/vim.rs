//! Pure Vim state machine: keys in, edits/cursor/selection/commands out. No UI.
//!
//! All offsets are UTF-8 byte offsets on char boundaries. Every operation only
//! touches the lines/chars around the cursor (plus O(log n) rope lookups).

use gpui_kit::component::{Rope, RopeExt};
use std::ops::Range;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    #[default]
    Normal,
    Insert,
    Visual,
    VisualLine,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmd {
    Undo,
    Redo,
    Search,
    SearchNext,
    SearchPrev,
    Save,
    /// `:wq`/`:x`: save, then close once the write lands.
    SaveQuit,
    Quit,
    /// `:q!`: close and drop unsaved edits.
    ForceQuit,
    PageDown,
    PageUp,
    Indent,
    Outdent,
}

#[derive(Default, Debug, PartialEq)]
pub struct Out {
    pub handled: bool,
    pub edit: Option<(Range<usize>, String)>,
    pub cursor: Option<usize>,
    pub select: Option<Range<usize>>,
    pub yank: Option<String>,
    pub cmd: Option<Cmd>,
}

#[derive(Default)]
pub struct Vim {
    pub mode: Mode,
    /// Count typed before the operator (or before a plain motion).
    count: Option<usize>,
    /// Count typed before the operator, once an operator is pending.
    op_count: Option<usize>,
    op: Option<char>,
    g: bool,
    replace: bool,
    cmdline: bool,
    /// Pending keys for display ("2d", "g", ":w").
    keys: String,
    /// Preferred char column for j/k; usize::MAX = end of line.
    want: Option<usize>,
    /// Visual-mode anchor.
    anchor: usize,
    /// Visual-mode cursor. The editor's caret sits at the selection's end, not on the
    /// cursor, so Vim keeps its own while a selection is shown.
    vcur: Option<usize>,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Exclusive,
    Inclusive,
    Linewise,
}

const MAX_COUNT: usize = 1_000_000;

impl Vim {
    pub fn status(&self) -> &'static str {
        match self.mode {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Visual => "VISUAL",
            Mode::VisualLine => "V-LINE",
        }
    }

    /// Nearest valid cursor for the current mode (unchanged in Insert mode).
    pub fn clamp(&self, text: &Rope, cursor: usize) -> usize {
        if self.mode == Mode::Insert {
            cursor
        } else {
            clamp(text, cursor)
        }
    }

    pub fn pending(&self) -> &str {
        &self.keys
    }

    fn visual(&self) -> bool {
        matches!(self.mode, Mode::Visual | Mode::VisualLine)
    }

    fn reset(&mut self) {
        self.count = None;
        self.op_count = None;
        self.op = None;
        self.g = false;
        self.replace = false;
        self.cmdline = false;
        self.keys.clear();
    }

    pub fn key(&mut self, key: &str, text: &Rope, cursor: usize, clipboard: Option<&str>) -> Out {
        if key.starts_with("cmd-") {
            return Out::default();
        }
        let t = text;
        let cur = t.floor_char_boundary(cursor.min(t.len()));
        let esc = key == "escape" || key == "ctrl-[";
        let cur = match (self.mode, self.vcur) {
            (Mode::Insert, _) => cur,
            (Mode::Visual | Mode::VisualLine, Some(v)) => clamp(t, v.min(t.len())),
            _ => clamp(t, cur),
        };
        // A reload can replace the text under a Visual selection.
        self.anchor = clamp(t, self.anchor);
        if self.mode == Mode::Insert {
            if !esc {
                return Out::default();
            }
            self.mode = Mode::Normal;
            self.reset();
            let s = lstart(t, row(t, cur));
            let c = if cur > s { prev(t, cur) } else { cur };
            return Out {
                handled: true,
                cursor: Some(clamp(t, c)),
                ..Out::default()
            };
        }
        let mut out = if self.cmdline {
            self.cmdline_key(key, esc)
        } else {
            self.normal(key, esc, t, cur, clipboard)
        };
        out.handled = true;
        self.vcur = None;
        if self.visual() {
            let c = *out.cursor.get_or_insert(cur);
            self.vcur = Some(c);
            if out.select.is_none() {
                out.select = Some(self.vis_range(t, c));
            }
        }
        out
    }

    fn cmdline_key(&mut self, key: &str, esc: bool) -> Out {
        let mut out = Out::default();
        match key {
            _ if esc => self.reset(),
            "backspace" => {
                if self.keys.len() <= 1 {
                    self.reset();
                } else {
                    self.keys.pop();
                }
            }
            "enter" => {
                out.cmd = match &self.keys[1..] {
                    "w" | "w!" => Some(Cmd::Save),
                    "wq" | "wq!" | "x" | "x!" => Some(Cmd::SaveQuit),
                    "q" => Some(Cmd::Quit),
                    "q!" => Some(Cmd::ForceQuit),
                    _ => None,
                };
                self.reset();
            }
            k if k.chars().count() == 1 => self.keys.push_str(k),
            _ => {}
        }
        out
    }

    fn vis_range(&self, t: &Rope, c: usize) -> Range<usize> {
        let (a, b) = (self.anchor.min(c), self.anchor.max(c));
        if self.mode == Mode::VisualLine {
            let r1 = row(t, b);
            let end = if r1 + 1 < nlines(t) {
                lstart(t, r1 + 1)
            } else {
                t.len()
            };
            lstart(t, row(t, a))..end
        } else {
            a..next(t, b)
        }
    }

    fn normal(&mut self, key: &str, esc: bool, t: &Rope, cur: usize, clip: Option<&str>) -> Out {
        let cursor = |c: usize| Out {
            cursor: Some(c),
            ..Out::default()
        };
        if esc {
            self.reset();
            if self.visual() {
                self.mode = Mode::Normal;
                return cursor(clamp(t, cur));
            }
            return Out::default();
        }
        let mut cs = key.chars();
        let ch = match (cs.next(), cs.next()) {
            (Some(c), None) => c,
            _ => {
                let cmd = match key {
                    "ctrl-d" => Some(Cmd::PageDown),
                    "ctrl-u" => Some(Cmd::PageUp),
                    "ctrl-r" if self.op.is_none() && !self.visual() => Some(Cmd::Redo),
                    _ => None,
                };
                self.reset();
                return Out {
                    cmd,
                    ..Out::default()
                };
            }
        };

        if self.replace {
            let n = self.count.unwrap_or(1);
            self.reset();
            return replace_chars(t, cur, n, ch);
        }

        // Counts.
        if ch.is_ascii_digit() && (ch != '0' || self.count.is_some()) && !self.g {
            let d = ch as usize - '0' as usize;
            self.count = Some((self.count.unwrap_or(0) * 10 + d).min(MAX_COUNT));
            self.keys.push(ch);
            return Out::default();
        }
        let has_count = self.count.is_some() || self.op_count.is_some();
        let n = (self.op_count.unwrap_or(1) * self.count.unwrap_or(1)).clamp(1, MAX_COUNT);

        if ch == 'g' && !self.g {
            self.g = true;
            self.keys.push(ch);
            return Out::default();
        }
        if self.g && ch != 'g' {
            self.reset();
            return Out::default();
        }
        let gg = self.g;

        // Operator pending.
        if let Some(op) = self.op {
            self.reset();
            if ch == op && !gg {
                let r0 = row(t, cur);
                let r1 = (r0 + n - 1).min(nlines(t) - 1);
                let target = lstart(t, r1);
                return self.apply(op, t, cur, target, Kind::Linewise);
            }
            if op == 'c'
                && matches!(ch, 'w' | 'W')
                && char_at(t, cur).is_some_and(|c| !c.is_whitespace())
            {
                // cw behaves like ce, but stays put on a word's last char.
                let big = ch == 'W';
                let mut p = cur;
                for i in 0..n {
                    let at_end = char_at(t, next(t, p))
                        .is_none_or(|c| class(c, big) != class(t.char(p), big));
                    if !(i == 0 && at_end) {
                        p = word_end(t, p, big);
                    }
                }
                return self.apply(op, t, cur, p, Kind::Inclusive);
            }
            return match self.motion(t, cur, ch, n, has_count, gg, true) {
                Some((p, kind)) => self.apply(op, t, cur, p, kind),
                None => Out::default(),
            };
        }

        if self.visual() {
            match ch {
                'd' | 'x' | 'y' | 'c' | '>' | '<' => {
                    self.reset();
                    let line = self.mode == Mode::VisualLine;
                    self.mode = Mode::Normal;
                    let (a, b) = (self.anchor.min(cur), self.anchor.max(cur));
                    let op = if ch == 'x' { 'd' } else { ch };
                    let mut out = if line {
                        self.apply(op, t, a, b, Kind::Linewise)
                    } else {
                        self.apply(op, t, a, b, Kind::Inclusive)
                    };
                    if op == 'y' {
                        out.cursor = Some(clamp(t, a));
                    }
                    return out;
                }
                'o' => {
                    self.reset();
                    let c = self.anchor;
                    self.anchor = cur;
                    return cursor(c);
                }
                'v' | 'V' => {
                    self.reset();
                    let m = if ch == 'v' {
                        Mode::Visual
                    } else {
                        Mode::VisualLine
                    };
                    self.mode = if self.mode == m { Mode::Normal } else { m };
                    return cursor(cur);
                }
                _ => {}
            }
        }

        if let Some((p, _)) = self.motion(t, cur, ch, n, has_count, gg, false) {
            self.reset();
            return cursor(clamp(t, p));
        }

        let r = row(t, cur);
        let (s, e) = (lstart(t, r), lend(t, r));
        let insert = |v: &mut Vim, c: usize| {
            v.mode = Mode::Insert;
            Out {
                cursor: Some(c),
                ..Out::default()
            }
        };
        let simple = !self.visual();
        let pend = |v: &mut Vim, op: char| {
            v.op = Some(op);
            v.op_count = v.count.take();
            v.keys.push(op);
            Out::default()
        };
        let out = match ch {
            'd' | 'c' | 'y' | '>' | '<' if simple => return pend(self, ch),
            'r' if simple => {
                self.replace = true;
                self.keys.push('r');
                return Out::default();
            }
            ':' if simple => {
                self.reset();
                self.cmdline = true;
                self.keys.push(':');
                return Out::default();
            }
            'i' if simple => insert(self, cur),
            'a' if simple => insert(self, if e > s { next(t, cur).min(e) } else { cur }),
            'I' if simple => insert(self, first_nonblank(t, r)),
            'A' if simple => insert(self, e),
            'o' | 'O' if simple => {
                let ind = indent(t, r);
                // Keep CRLF files CRLF.
                let nl = if t.line_end_offset(r) > e {
                    "\r\n"
                } else {
                    "\n"
                };
                self.mode = Mode::Insert;
                if ch == 'o' {
                    let c = e + nl.len() + ind.len();
                    Out {
                        edit: Some((e..e, format!("{nl}{ind}"))),
                        cursor: Some(c),
                        ..Out::default()
                    }
                } else {
                    let c = s + ind.len();
                    Out {
                        edit: Some((s..s, format!("{ind}{nl}"))),
                        cursor: Some(c),
                        ..Out::default()
                    }
                }
            }
            'x' | 'X' | 'D' | 'C' | 's' | 'S' if simple => {
                let (op, m) = match ch {
                    'x' => ('d', 'l'),
                    'X' => ('d', 'h'),
                    'D' => ('d', '$'),
                    'C' => ('c', '$'),
                    's' => ('c', 'l'),
                    _ => ('c', 'c'),
                };
                if m == 'c' {
                    let r1 = (r + n - 1).min(nlines(t) - 1);
                    self.reset();
                    return self.apply(op, t, cur, lstart(t, r1), Kind::Linewise);
                }
                if m == 'l' && e == s {
                    if op == 'c' {
                        self.reset();
                        return insert(self, cur);
                    }
                    Out::default()
                } else {
                    let n = if m == '$' { 1 } else { n };
                    let (p, kind) = self.motion(t, cur, m, n, false, false, true).unwrap();
                    self.reset();
                    return self.apply(op, t, cur, p, kind);
                }
            }
            'p' | 'P' if simple => paste(t, cur, n, ch == 'p', clip),
            'J' if simple => join(t, r, n.max(2)),
            '~' if simple => {
                let mut end = cur;
                for _ in 0..n {
                    if end >= e {
                        break;
                    }
                    end = next(t, end);
                }
                let s: String = t
                    .slice(cur..end)
                    .chars()
                    .map(|c| {
                        if c.is_lowercase() {
                            c.to_uppercase().next().unwrap()
                        } else {
                            c.to_lowercase().next().unwrap()
                        }
                    })
                    .collect();
                if cur == end {
                    Out::default()
                } else {
                    let last = s.chars().next_back().map_or(0, char::len_utf8);
                    let c = if end >= e {
                        cur + s.len() - last
                    } else {
                        cur + s.len()
                    };
                    Out {
                        cursor: Some(c),
                        edit: Some((cur..end, s)),
                        ..Out::default()
                    }
                }
            }
            'u' if simple => Out {
                cmd: Some(Cmd::Undo),
                ..Out::default()
            },
            '/' => Out {
                cmd: Some(Cmd::Search),
                ..Out::default()
            },
            'n' => Out {
                cmd: Some(Cmd::SearchNext),
                ..Out::default()
            },
            'N' => Out {
                cmd: Some(Cmd::SearchPrev),
                ..Out::default()
            },
            'v' | 'V' => {
                self.mode = if ch == 'v' {
                    Mode::Visual
                } else {
                    Mode::VisualLine
                };
                self.anchor = cur;
                cursor(cur)
            }
            _ => Out::default(),
        };
        self.reset();
        out
    }

    /// Resolve a motion. `op`: called from operator-pending mode.
    #[allow(clippy::too_many_arguments)]
    fn motion(
        &mut self,
        t: &Rope,
        cur: usize,
        ch: char,
        n: usize,
        has_count: bool,
        gg: bool,
        op: bool,
    ) -> Option<(usize, Kind)> {
        use Kind::*;
        let r = row(t, cur);
        let last = nlines(t) - 1;
        let col_keep = !matches!(ch, 'j' | 'k') || gg;
        let res = match ch {
            'g' if gg => {
                let rr = if has_count { (n - 1).min(last) } else { 0 };
                (first_nonblank(t, rr), Linewise)
            }
            'G' => {
                let rr = if has_count { (n - 1).min(last) } else { last };
                (first_nonblank(t, rr), Linewise)
            }
            'h' => {
                let s = lstart(t, r);
                let mut p = cur;
                for _ in 0..n {
                    if p <= s {
                        break;
                    }
                    p = prev(t, p);
                }
                (p, Exclusive)
            }
            'l' => {
                let e = lend(t, r);
                let mut p = cur;
                for _ in 0..n {
                    if p >= e {
                        break;
                    }
                    p = next(t, p);
                }
                (p, Exclusive)
            }
            'j' | 'k' => {
                let rr = if ch == 'j' {
                    (r + n).min(last)
                } else {
                    r.saturating_sub(n)
                };
                if rr == r && op {
                    return None;
                }
                let want = *self.want.get_or_insert_with(|| col_of(t, cur));
                (at_col(t, rr, want), Linewise)
            }
            'w' | 'W' => {
                let big = ch == 'W';
                let mut p = cur;
                for i in 0..n {
                    let q = word_fwd(t, p, big);
                    // `dw` on a line's last word stops at end of line.
                    if op && i == n - 1 && row(t, q) > row(t, p) {
                        p = lend(t, row(t, p)).max(p);
                        break;
                    }
                    if q == p {
                        break;
                    }
                    p = q;
                }
                (p, Exclusive)
            }
            'b' | 'B' => {
                let mut p = cur;
                for _ in 0..n {
                    p = word_back(t, p, ch == 'B');
                }
                (p, Exclusive)
            }
            'e' | 'E' => {
                let mut p = cur;
                for _ in 0..n {
                    p = word_end(t, p, ch == 'E');
                }
                (p, Inclusive)
            }
            '0' => (lstart(t, r), Exclusive),
            '^' => (first_nonblank(t, r), Exclusive),
            '$' => {
                self.want = Some(usize::MAX);
                return Some((
                    lend(t, (r + n - 1).min(last)),
                    if op { Exclusive } else { Inclusive },
                ));
            }
            '}' | '{' => {
                let mut rr = r;
                let mut p = cur;
                for _ in 0..n {
                    (p, rr) = if ch == '}' {
                        para_fwd(t, rr)
                    } else {
                        para_back(t, rr)
                    };
                }
                (p, Exclusive)
            }
            _ => return None,
        };
        if col_keep {
            self.want = None;
        }
        Some(res)
    }

    /// Apply operator `op` over cursor..target.
    fn apply(&mut self, op: char, t: &Rope, cur: usize, target: usize, kind: Kind) -> Out {
        let (a, b) = (cur.min(target), cur.max(target));
        if kind == Kind::Linewise || matches!(op, '>' | '<') {
            let (r0, r1) = (row(t, a), row(t, b));
            let lines = || format!("{}\n", t.slice(lstart(t, r0)..lend(t, r1)));
            return match op {
                'd' => {
                    // Raw count: a trailing '\n' is deleted with the last real line.
                    let range = if r1 + 1 < t.lines_len() {
                        lstart(t, r0)..lstart(t, r1 + 1)
                    } else if r0 > 0 {
                        lend(t, r0 - 1)..t.len()
                    } else {
                        0..t.len()
                    };
                    let c = if r1 + 1 < nlines(t) {
                        first_nonblank(t, r1 + 1) - range.len()
                    } else if r0 > 0 {
                        first_nonblank(t, r0 - 1)
                    } else {
                        0
                    };
                    Out {
                        yank: Some(lines()),
                        edit: Some((range, String::new())),
                        cursor: Some(c),
                        ..Out::default()
                    }
                }
                'c' => {
                    let ind = indent(t, r0);
                    let s = lstart(t, r0);
                    self.mode = Mode::Insert;
                    Out {
                        yank: Some(lines()),
                        cursor: Some(s + ind.len()),
                        edit: Some((s..lend(t, r1), ind)),
                        ..Out::default()
                    }
                }
                'y' => {
                    let c = if row(t, target) < row(t, cur) {
                        target
                    } else {
                        cur
                    };
                    Out {
                        yank: Some(lines()),
                        cursor: Some(clamp(t, c)),
                        ..Out::default()
                    }
                }
                _ => Out {
                    select: Some(lstart(t, r0)..lend(t, r1)),
                    cmd: Some(if op == '>' { Cmd::Indent } else { Cmd::Outdent }),
                    ..Out::default()
                },
            };
        }
        let b = if kind == Kind::Inclusive {
            next(t, b)
        } else {
            b
        };
        let yank = Some(t.slice(a..b).to_string());
        match op {
            'd' => {
                if a == b {
                    return Out {
                        cursor: Some(clamp(t, a)),
                        ..Out::default()
                    };
                }
                // Clamp in post-edit text: if the deletion reaches end of line, step left.
                let eol = b >= lend(t, row(t, b));
                let c = if eol && a > lstart(t, row(t, a)) {
                    prev(t, a)
                } else {
                    a
                };
                Out {
                    yank,
                    edit: Some((a..b, String::new())),
                    cursor: Some(c),
                    ..Out::default()
                }
            }
            'c' => {
                self.mode = Mode::Insert;
                let edit = (a != b).then(|| (a..b, String::new()));
                Out {
                    yank,
                    edit,
                    cursor: Some(a),
                    ..Out::default()
                }
            }
            _ => Out {
                yank,
                cursor: Some(clamp(t, a)),
                ..Out::default()
            },
        }
    }
}

fn replace_chars(t: &Rope, cur: usize, n: usize, ch: char) -> Out {
    let e = lend(t, row(t, cur));
    let mut end = cur;
    for _ in 0..n {
        if end >= e {
            return Out::default();
        }
        end = next(t, end);
    }
    let s: String = std::iter::repeat_n(ch, n).collect();
    let c = cur + s.len() - ch.len_utf8();
    Out {
        edit: Some((cur..end, s)),
        cursor: Some(c),
        ..Out::default()
    }
}

fn paste(t: &Rope, cur: usize, n: usize, after: bool, clip: Option<&str>) -> Out {
    let Some(clip) = clip.filter(|c| !c.is_empty()) else {
        return Out::default();
    };
    let r = row(t, cur);
    if clip.ends_with('\n') {
        let body = clip.repeat(n);
        let (at, text, c) = if !after {
            let s = lstart(t, r);
            (s, body, s)
        } else if r + 1 < t.lines_len() {
            let s = lstart(t, r + 1);
            (s, body, s)
        } else {
            let e = t.len();
            let mut text = String::with_capacity(body.len());
            text.push('\n');
            text.push_str(&body[..body.len() - 1]);
            (e, text, e + 1)
        };
        let ind = clip.len() - clip.trim_start_matches([' ', '\t']).len();
        let ind = if clip[..ind].contains('\n') { 0 } else { ind };
        return Out {
            edit: Some((at..at, text)),
            cursor: Some(c + ind),
            ..Out::default()
        };
    }
    let at = if after && lend(t, r) > lstart(t, r) {
        next(t, cur)
    } else {
        cur
    };
    let text = clip.repeat(n);
    let last = text.chars().next_back().map_or(0, char::len_utf8);
    let c = at + text.len() - last;
    Out {
        edit: Some((at..at, text)),
        cursor: Some(c),
        ..Out::default()
    }
}

fn join(t: &Rope, r: usize, n: usize) -> Out {
    let last = (r + n - 1).min(nlines(t) - 1);
    if last == r {
        return Out::default();
    }
    let start = lend(t, r);
    let mut text = String::new();
    let mut prev_empty = lend(t, r) == lstart(t, r);
    let mut sep_at = 0;
    for rr in r + 1..=last {
        let fnb = first_nonblank(t, rr);
        let content = t.slice(fnb..lend(t, rr));
        sep_at = text.len();
        if !prev_empty && content.len() > 0 {
            text.push(' ');
        }
        if rr < last {
            text.push_str(&content.to_string());
        }
        prev_empty = content.len() == 0 && prev_empty;
    }
    let end = first_nonblank(t, last);
    let mut c = start + sep_at;
    // Joined an empty last line: keep the cursor off the '\n'.
    if c == start + text.len() && end == lend(t, last) && c > lstart(t, r) {
        c = if text.is_empty() {
            prev(t, start)
        } else {
            c - text.chars().next_back().map_or(0, char::len_utf8)
        };
    }
    Out {
        edit: Some((start..end, text)),
        cursor: Some(c),
        ..Out::default()
    }
}

// ---- rope helpers (all O(log n) or local) ----

fn row(t: &Rope, off: usize) -> usize {
    t.offset_to_point(off).row
}

fn lstart(t: &Rope, r: usize) -> usize {
    t.line_start_offset(r)
}

/// End of line `r`, excluding the '\n' and a CRLF's '\r'.
fn lend(t: &Rope, r: usize) -> usize {
    let e = t.line_end_offset(r);
    if e > lstart(t, r) && t.byte(e - 1) == b'\r' {
        e - 1
    } else {
        e
    }
}

fn char_at(t: &Rope, off: usize) -> Option<char> {
    (off < t.len()).then(|| t.char(off))
}

fn next(t: &Rope, off: usize) -> usize {
    char_at(t, off).map_or(t.len(), |c| off + c.len_utf8())
}

fn prev(t: &Rope, off: usize) -> usize {
    if off == 0 {
        0
    } else {
        t.chars_at(off).prev().map_or(0, |c| off - c.len_utf8())
    }
}

/// Lines as Vim counts them: a trailing '\n' does not start another line.
fn nlines(t: &Rope) -> usize {
    let n = t.lines_len();
    if n > 1 && t.byte(t.len() - 1) == b'\n' {
        n - 1
    } else {
        n
    }
}

/// Normal-mode cursor: never on a line's '\n' unless the line is empty, never
/// on the pseudo-line after a trailing '\n'.
fn clamp(t: &Rope, off: usize) -> usize {
    let mut off = t.floor_char_boundary(off.min(t.len()));
    let mut r = row(t, off);
    if r >= nlines(t) {
        r = nlines(t) - 1;
        off = lend(t, r);
    }
    let (s, e) = (lstart(t, r), lend(t, r));
    if e == s { s } else { off.clamp(s, prev(t, e)) }
}

fn first_nonblank(t: &Rope, r: usize) -> usize {
    let (s, e) = (lstart(t, r), lend(t, r));
    let mut p = s;
    for c in t.slice(s..e).chars() {
        if c != ' ' && c != '\t' {
            break;
        }
        p += c.len_utf8();
    }
    p
}

fn indent(t: &Rope, r: usize) -> String {
    let s = lstart(t, r);
    t.slice(s..first_nonblank(t, r)).to_string()
}

fn col_of(t: &Rope, off: usize) -> usize {
    t.slice(lstart(t, row(t, off))..off).chars().count()
}

/// Offset of char column `col` in line `r`, clamped to the last char.
fn at_col(t: &Rope, r: usize, col: usize) -> usize {
    let (s, e) = (lstart(t, r), lend(t, r));
    let mut p = s;
    for (i, c) in t.slice(s..e).chars().enumerate() {
        if i == col || p + c.len_utf8() >= e {
            break;
        }
        p += c.len_utf8();
    }
    p
}

fn class(c: char, big: bool) -> u8 {
    if c.is_whitespace() {
        0
    } else if big || c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

fn word_fwd(t: &Rope, p: usize, big: bool) -> usize {
    let mut it = t.char_indices_at(p);
    let Some((_, c0)) = it.next() else {
        return t.len();
    };
    let k0 = class(c0, big);
    let mut saw_nl = c0 == '\n';
    let (mut i, mut c) = loop {
        match it.next() {
            None => return t.len(),
            Some((_, c)) if k0 != 0 && class(c, big) == k0 => {}
            Some(x) => break x,
        }
    };
    loop {
        if c == '\n' {
            if saw_nl {
                return i;
            }
            saw_nl = true;
        } else if class(c, big) != 0 {
            return i;
        }
        match it.next() {
            None => return t.len(),
            Some(x) => (i, c) = x,
        }
    }
}

fn word_end(t: &Rope, p: usize, big: bool) -> usize {
    let mut it = t.char_indices_at(p);
    it.next();
    let mut cur = loop {
        match it.next() {
            // Never the trailing '\n': `de` on the last word would eat it.
            None => return clamp(t, prev(t, t.len())).max(p),
            Some((_, c)) if c.is_whitespace() => {}
            Some(x) => break x,
        }
    };
    let k = class(cur.1, big);
    loop {
        match it.next() {
            Some((i, c)) if class(c, big) == k => cur = (i, c),
            _ => return cur.0,
        }
    }
}

fn word_back(t: &Rope, p: usize, big: bool) -> usize {
    let mut it = t.char_indices_at(p);
    let (mut i, c) = loop {
        match it.prev() {
            None => return 0,
            Some((i, '\n')) => {
                if i == 0 || t.byte(i - 1) == b'\n' {
                    return i;
                }
            }
            Some((_, c)) if c.is_whitespace() => {}
            Some(x) => break x,
        }
    };
    let k = class(c, big);
    loop {
        match it.prev() {
            Some((j, d)) if class(d, big) == k => i = j,
            _ => return i,
        }
    }
}

fn blank(t: &Rope, r: usize) -> bool {
    lstart(t, r) == lend(t, r)
}

fn para_fwd(t: &Rope, mut r: usize) -> (usize, usize) {
    let n = nlines(t);
    while r + 1 < n && blank(t, r) {
        r += 1;
    }
    while r + 1 < n && !blank(t, r) {
        r += 1;
    }
    (
        if blank(t, r) {
            lstart(t, r)
        } else {
            lend(t, r)
        },
        r,
    )
}

fn para_back(t: &Rope, mut r: usize) -> (usize, usize) {
    while r > 0 && blank(t, r) {
        r -= 1;
    }
    while r > 0 && !blank(t, r) {
        r -= 1;
    }
    (lstart(t, r), r)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Run {
        text: String,
        vim: Vim,
        last: Out,
        clip: Option<String>,
    }

    fn split(keys: &str) -> Vec<String> {
        let mut v = Vec::new();
        let mut cs = keys.chars().peekable();
        while let Some(c) = cs.next() {
            if c == '<' && cs.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
                let mut name = String::new();
                for d in cs.by_ref() {
                    if d == '>' {
                        break;
                    }
                    name.push(d);
                }
                v.push(match name.as_str() {
                    "esc" => "escape".into(),
                    "cr" => "enter".into(),
                    "bs" => "backspace".into(),
                    n if n.starts_with("C-") => format!("ctrl-{}", &n[2..]),
                    n if n.starts_with("D-") => format!("cmd-{}", &n[2..]),
                    n => panic!("unknown key <{n}>"),
                });
            } else {
                v.push(c.to_string());
            }
        }
        v
    }

    fn run_with(src: &str, keys: &str, clip: Option<&str>) -> Run {
        run_caret(src, keys, clip, false)
    }

    /// `editor_caret`: feed back the caret the way the editor reports it, at the end of the
    /// shown selection, instead of `Out::cursor`.
    fn run_caret(src: &str, keys: &str, clip: Option<&str>, editor_caret: bool) -> Run {
        let cur = src.find('|').expect("cursor");
        let mut rope = Rope::from(src.replacen('|', "", 1).as_str());
        let mut cur = cur;
        let mut vim = Vim::default();
        let mut clip = clip.map(String::from);
        let mut last = Out::default();
        for k in split(keys) {
            let out = vim.key(&k, &rope, cur, clip.as_deref());
            if !out.handled {
                if vim.mode == Mode::Insert && k.chars().count() == 1 {
                    rope.insert(cur, &k);
                    cur += k.len();
                }
                last = out;
                continue;
            }
            if let Some((r, s)) = &out.edit {
                assert!(
                    r.start <= r.end && r.end <= rope.len(),
                    "edit {r:?} out of bounds"
                );
                assert!(rope.is_char_boundary(r.start) && rope.is_char_boundary(r.end));
                rope.remove(r.clone());
                rope.insert(r.start, s);
                assert!(out.cursor.is_some(), "edit without cursor");
            }
            if let Some(c) = out.cursor {
                assert!(
                    c <= rope.len() && rope.is_char_boundary(c),
                    "cursor {c} bad"
                );
                cur = c;
            }
            if let Some(y) = &out.yank {
                clip = Some(y.clone());
            }
            if editor_caret && let Some(sel) = &out.select {
                cur = sel.end;
            }
            if vim.mode == Mode::Normal && cur < rope.len() && rope.byte(cur) == b'\n' {
                assert!(
                    cur == 0 || rope.byte(cur - 1) == b'\n',
                    "normal cursor on newline at {cur}"
                );
            }
            last = out;
        }
        let mut text = rope.to_string();
        text.insert(cur, '|');
        Run {
            text,
            vim,
            last,
            clip,
        }
    }

    fn run(src: &str, keys: &str) -> Run {
        run_with(src, keys, None)
    }

    fn k(src: &str, keys: &str) -> String {
        run(src, keys).text
    }

    #[test]
    fn hjkl_and_preferred_column() {
        assert_eq!(k("ab|c", "h"), "a|bc");
        assert_eq!(k("|abc", "h"), "|abc");
        assert_eq!(k("|abc", "l"), "a|bc");
        assert_eq!(k("ab|c\nd", "l"), "ab|c\nd");
        assert_eq!(k("|abcdef", "3l"), "abc|def");
        assert_eq!(k("abcd|ef\nx\nabcdef", "j"), "abcdef\n|x\nabcdef");
        assert_eq!(k("abcd|ef\nx\nabcdef", "jj"), "abcdef\nx\nabcd|ef");
        assert_eq!(k("abcd|ef\nx\n\nabcdef", "3jk"), "abcdef\nx\n|\nabcdef");
        assert_eq!(k("a\nb\nc\n|d", "2k"), "a\n|b\nc\nd");
        assert_eq!(k("a\nb|c", "5k"), "|a\nbc");
        assert_eq!(k("ab|c\nd", "$j"), "abc\n|d");
        assert_eq!(k("|abc\nabcdef", "$j"), "abc\nabcde|f");
    }

    #[test]
    fn words() {
        assert_eq!(k("|foo bar", "w"), "foo |bar");
        assert_eq!(k("|foo.bar baz", "w"), "foo|.bar baz");
        assert_eq!(k("|foo.bar baz", "W"), "foo.bar |baz");
        assert_eq!(k("|foo\n  bar", "w"), "foo\n  |bar");
        assert_eq!(k("|foo\n\nbar", "w"), "foo\n|\nbar");
        assert_eq!(k("|a b c d", "3w"), "a b c |d");
        assert_eq!(k("foo |bar", "w"), "foo ba|r");
        assert_eq!(k("foo b|ar", "b"), "foo |bar");
        assert_eq!(k("foo |bar", "b"), "|foo bar");
        assert_eq!(k("foo\n\n|bar", "b"), "foo\n|\nbar");
        assert_eq!(k("a.b |c", "B"), "|a.b c");
        assert_eq!(k("|foo bar", "e"), "fo|o bar");
        assert_eq!(k("fo|o bar", "e"), "foo ba|r");
        assert_eq!(k("|a.b c", "E"), "a.|b c");
        assert_eq!(k("|ab cd ef", "2e"), "ab c|d ef");
        assert_eq!(k("|é😀x yz", "w"), "é|😀x yz");
        assert_eq!(k("|éé ü", "e"), "é|é ü");
        assert_eq!(k("éé |ü", "b"), "|éé ü");
        assert_eq!(k("x |😀😀 y", "w"), "x 😀😀 |y");
    }

    #[test]
    fn line_motions() {
        assert_eq!(k("  ab|c", "0"), "|  abc");
        assert_eq!(k("  ab|c", "^"), "  |abc");
        assert_eq!(k("|abc\n", "$"), "ab|c\n");
        assert_eq!(k("|aé", "$"), "a|é");
        assert_eq!(k("a\n  b\n|c", "gg"), "|a\n  b\nc");
        assert_eq!(k("|a\n  b\nc", "G"), "a\n  b\n|c");
        assert_eq!(k("|a\n  b\nc", "2G"), "a\n  |b\nc");
        assert_eq!(k("|a\n  b\nc", "2gg"), "a\n  |b\nc");
        assert_eq!(k("|a\n  b\nc", "99G"), "a\n  b\n|c");
        assert_eq!(k("|a\nb\n\nc\n\nd", "}"), "a\nb\n|\nc\n\nd");
        assert_eq!(k("|a\nb\n\nc\n\nd", "2}"), "a\nb\n\nc\n|\nd");
        assert_eq!(k("|a\nb\n\ncd", "3}"), "a\nb\n\nc|d");
        assert_eq!(k("a\n\nb\n|c", "{"), "a\n|\nb\nc");
        assert_eq!(k("a\nb\n|c", "{"), "|a\nb\nc");
        assert_eq!(run("|a", "<C-d>").last.cmd, Some(Cmd::PageDown));
        assert_eq!(run("|a", "<C-u>").last.cmd, Some(Cmd::PageUp));
    }

    #[test]
    fn insert_entry() {
        assert_eq!(k("a|bc", "iX<esc>"), "a|Xbc");
        assert_eq!(k("a|bc", "aX<esc>"), "ab|Xc");
        assert_eq!(k("  a|bc", "IX<esc>"), "  |Xabc");
        assert_eq!(k("a|bc", "AX<esc>"), "abc|X");
        assert_eq!(k("|", "aX<esc>"), "|X");
        assert_eq!(k("  a|b\nc", "oX<esc>"), "  ab\n  |X\nc");
        assert_eq!(k("a\n\t|b", "OX<esc>"), "a\n\t|X\n\tb");
        assert_eq!(k("ab|c", "i<esc>"), "a|bc");
        assert_eq!(k("|abc", "i<esc>"), "|abc");
        let r = run("|a", "i");
        assert_eq!(r.vim.status(), "INSERT");
        assert!(!run("|a", "ix").last.handled);
        let mut v = Vim::default();
        assert!(!v.key("cmd-s", &Rope::from("a"), 0, None).handled);
        v.mode = Mode::Insert;
        assert!(!v.key("x", &Rope::from("a"), 0, None).handled);
        assert!(!v.key("enter", &Rope::from("a"), 0, None).handled);
        assert!(v.key("ctrl-[", &Rope::from("a"), 0, None).handled);
        assert_eq!(v.mode, Mode::Normal);
    }

    #[test]
    fn x_d_c() {
        assert_eq!(k("a|bc", "x"), "a|c");
        assert_eq!(k("ab|c", "x"), "a|b");
        assert_eq!(k("a|bcd", "2x"), "a|d");
        assert_eq!(k("a|bcd", "9x"), "|a");
        assert_eq!(k("|é😀x", "x"), "|😀x");
        assert_eq!(k("|é😀x", "lx"), "é|x");
        assert_eq!(k("|\nb", "x"), "|\nb");
        assert_eq!(k("ab|c", "X"), "a|c");
        assert_eq!(k("a|bc d", "D"), "|a");
        assert_eq!(k("a|bc d", "Cx<esc>"), "a|x");
        assert_eq!(k("a|bc", "sX<esc>"), "a|Xc");
        assert_eq!(k("  a|bc\nd", "SX<esc>"), "  |X\nd");
        assert_eq!(k("|foo bar", "dw"), "|bar");
        assert_eq!(k("foo |bar\nbaz", "dw"), "foo| \nbaz");
        assert_eq!(k("|a b c d", "d2w"), "|c d");
        assert_eq!(k("|a b c d e f g h", "2d3w"), "|g h");
        assert_eq!(k("|foo bar", "cwX<esc>"), "|X bar");
        assert_eq!(k("fo|o bar", "cwX<esc>"), "fo|X bar");
        assert_eq!(k("|a b c", "c2wX<esc>"), "|X c");
        assert_eq!(k("|foo bar", "de"), "| bar");
        assert_eq!(k("foo b|ar", "db"), "foo |ar");
        assert_eq!(k("ab|cd", "d0"), "|cd");
        assert_eq!(k("ab|cd", "d$"), "a|b");
        assert_eq!(k("a|bcd", "dl"), "a|cd");
        assert_eq!(k("ab|cd", "dh"), "a|cd");
        assert_eq!(k("a\n|b\nc\nd", "dj"), "a\n|d");
        assert_eq!(k("a\nb\n|c\nd", "dk"), "a\n|d");
        assert_eq!(k("a\n|b\nc", "dG"), "|a");
        assert_eq!(k("a\nb\n|c\nd", "dgg"), "|d");
        assert_eq!(k("a\n|b", "dj"), "a\n|b");
        assert_eq!(k("|a\nb\n\nc", "d}"), "|\nc");
        assert_eq!(k("ab|c", "d<esc>x"), "a|b");
        assert_eq!(k("ab|c", "dq"), "ab|c");
        assert_eq!(k("ab|c", "cl<esc>"), "a|b");
    }

    #[test]
    fn dd_cc_yy() {
        assert_eq!(k("a\n|b\nc", "dd"), "a\n|c");
        assert_eq!(k("a\nb\n|c", "dd"), "a\n|b");
        assert_eq!(k("|only", "dd"), "|");
        assert_eq!(k("|a\nb\nc\nd", "3dd"), "|d");
        assert_eq!(k("a\n|b\nc", "9dd"), "|a");
        assert_eq!(k("a\n|  b\n  c", "dd"), "a\n  |c");
        assert_eq!(k("a\n|", "dd"), "|");
        let r = run("a\n|b\nc", "dd");
        assert_eq!(r.clip.as_deref(), Some("b\n"));
        let r = run("a\nb\n|c", "dd");
        assert_eq!(r.clip.as_deref(), Some("c\n"));
        assert_eq!(k("a\n  |b\nc", "ccX<esc>"), "a\n  |X\nc");
        assert_eq!(k("|a\nb\nc", "2ccX<esc>"), "|X\nc");
        let r = run("a\n|b\nc", "yy");
        assert_eq!(r.clip.as_deref(), Some("b\n"));
        assert_eq!(r.text, "a\n|b\nc");
        let r = run("|a\nb\nc", "2yy");
        assert_eq!(r.clip.as_deref(), Some("a\nb\n"));
        let r = run("a b|c d", "yw");
        assert_eq!(r.clip.as_deref(), Some("c "));
        let r = run("a\nb|c", "yk");
        assert_eq!(r.clip.as_deref(), Some("a\nbc\n"));
        assert_eq!(r.text, "|a\nbc");
        assert_eq!(k("ab|cd", "y$"), "ab|cd");
        assert_eq!(run("ab|cd", "y$").clip.as_deref(), Some("cd"));
    }

    #[test]
    fn paste() {
        let p = |src: &str, keys: &str, clip: &str| run_with(src, keys, Some(clip)).text;
        assert_eq!(p("a|b", "p", "XY"), "abX|Y");
        assert_eq!(p("a|b", "P", "XY"), "aX|Yb");
        assert_eq!(p("a|b", "2p", "X"), "abX|X");
        assert_eq!(p("|", "p", "X"), "|X");
        assert_eq!(p("|ab", "p", "😀"), "a|😀b");
        assert_eq!(p("a|b\nc", "p", "  X\n"), "ab\n  |X\nc");
        assert_eq!(p("a|b\nc", "P", "X\n"), "|X\nab\nc");
        assert_eq!(p("a|b", "p", "X\n"), "ab\n|X");
        assert_eq!(p("a|b", "2p", "X\n"), "ab\n|X\nX");
        assert_eq!(p("a|b\n", "p", "X\n"), "ab\n|X\n");
        assert_eq!(k("a|b", "p"), "a|b");
        // yank-then-paste roundtrip
        assert_eq!(k("|a\nb", "yyjp"), "a\nb\n|a");
        assert_eq!(k("|a\nb", "ddp"), "b\n|a");
        assert_eq!(k("|ab", "xp"), "b|a");
    }

    #[test]
    fn join_replace_tilde_undo() {
        assert_eq!(k("|a\n   b", "J"), "a| b");
        assert_eq!(k("|a\nb\nc", "3J"), "a b| c");
        assert_eq!(k("|a\n\nc", "J"), "|a\nc");
        assert_eq!(k("|\nb", "J"), "|b");
        assert_eq!(k("|a", "J"), "|a");
        assert_eq!(k("|abc", "rX"), "|Xbc");
        assert_eq!(k("|abc", "2rX"), "X|Xc");
        assert_eq!(k("|abc", "5rX"), "|abc");
        assert_eq!(k("|abc", "r<esc>"), "|abc");
        assert_eq!(k("|é", "r😀"), "|😀");
        assert_eq!(k("|aBc", "~"), "A|Bc");
        assert_eq!(k("|aBc", "3~"), "Ab|C");
        assert_eq!(k("|é", "~"), "|É");
        assert_eq!(run("|a", "u").last.cmd, Some(Cmd::Undo));
        assert_eq!(run("|a", "<C-r>").last.cmd, Some(Cmd::Redo));
        assert_eq!(run("|a", "/").last.cmd, Some(Cmd::Search));
        assert_eq!(run("|a", "n").last.cmd, Some(Cmd::SearchNext));
        assert_eq!(run("|a", "N").last.cmd, Some(Cmd::SearchPrev));
        assert_eq!(k("|a", "q"), "|a");
        assert_eq!(k("|a", "<C-x>"), "|a");
    }

    #[test]
    fn indent_outdent() {
        let r = run("a\n|b\nc", ">>");
        assert_eq!(r.last.cmd, Some(Cmd::Indent));
        assert_eq!(r.last.select, Some(2..3));
        let r = run("|a\nb\nc", "2<<");
        assert_eq!(r.last.cmd, Some(Cmd::Outdent));
        assert_eq!(r.last.select, Some(0..3));
        let r = run("|a\nb\nc", ">j");
        assert_eq!(r.last.select, Some(0..3));
        let r = run("a\n|b\nc", "Vj>");
        assert_eq!((r.last.select, r.last.cmd), (Some(2..5), Some(Cmd::Indent)));
        assert_eq!(r.vim.mode, Mode::Normal);
    }

    #[test]
    fn visual() {
        let r = run("a|bcd", "v");
        assert_eq!(r.last.select, Some(1..2));
        assert_eq!(r.vim.status(), "VISUAL");
        let r = run("a|bcd", "vl");
        assert_eq!(r.last.select, Some(1..3));
        let r = run("ab|cd", "vh");
        assert_eq!(r.last.select, Some(1..3));
        let r = run("|éb", "v");
        assert_eq!(r.last.select, Some(0..2));
        assert_eq!(k("a|bcd", "vld"), "a|d");
        assert_eq!(k("a|bcd", "vlx"), "a|d");
        assert_eq!(k("ab|c", "vd"), "a|b");
        let r = run("a|bcd", "vly");
        assert_eq!((r.text.as_str(), r.clip.as_deref()), ("a|bcd", Some("bc")));
        assert_eq!(k("a|bcd", "vlcX<esc>"), "a|Xd");
        assert_eq!(k("a|bcd", "vl<esc>"), "ab|cd");
        assert_eq!(run("a|bcd", "vl<esc>").last.select, None);
        assert_eq!(k("a|bcd", "vlohd"), "|d");
        assert_eq!(k("foo |bar baz", "vwd"), "foo |az");
        let r = run("a\n|b\nc", "V");
        assert_eq!(r.last.select, Some(2..4));
        assert_eq!(r.vim.status(), "V-LINE");
        let r = run("a\n|b\nc", "Vj");
        assert_eq!(r.last.select, Some(2..5));
        assert_eq!(k("a\n|b\nc\nd", "Vjd"), "a\n|d");
        assert_eq!(k("a\n|b\nc", "Vjd"), "|a");
        let r = run("a\n|b\nc", "Vky");
        assert_eq!(
            (r.text.as_str(), r.clip.as_deref()),
            ("|a\nb\nc", Some("a\nb\n"))
        );
        assert_eq!(k("a\n  |b\nc", "VcX<esc>"), "a\n  |X\nc");
        assert_eq!(run("a|b", "vV").vim.mode, Mode::VisualLine);
        assert_eq!(run("a|b", "vv").vim.mode, Mode::Normal);
        let r = run("a\n|b\nc", "V2j");
        assert_eq!(r.last.select, Some(2..5));
        assert_eq!(run("a|b", "vu").vim.mode, Mode::Visual);
    }

    #[test]
    fn cmdline() {
        let r = run("|a", ":w");
        assert_eq!(r.vim.pending(), ":w");
        assert_eq!(run("|a", ":w<cr>").last.cmd, Some(Cmd::Save));
        assert_eq!(run("|a", ":wq<cr>").last.cmd, Some(Cmd::SaveQuit));
        assert_eq!(run("|a", ":x<cr>").last.cmd, Some(Cmd::SaveQuit));
        assert_eq!(run("|a", ":q<cr>").last.cmd, Some(Cmd::Quit));
        assert_eq!(run("|a", ":q!<cr>").last.cmd, Some(Cmd::ForceQuit));
        assert_eq!(run("|a", ":qa<cr>").last.cmd, None);
        assert_eq!(run("|a", ":w<esc>").vim.pending(), "");
        assert_eq!(run("|a", ":wx<bs><cr>").last.cmd, Some(Cmd::Save));
        let r = run("|a", ":<bs>");
        assert_eq!(r.vim.pending(), "");
        assert_eq!(k("|a", ":<bs>x"), "|");
        assert_eq!(k("|a", ":x"), "|a");
    }

    #[test]
    fn crlf_lines() {
        assert_eq!(k("a|b\r\nc\r\n", "A!<esc>"), "ab|!\r\nc\r\n");
        assert_eq!(k("|ab\r\nc\r\n", "$"), "a|b\r\nc\r\n");
        assert_eq!(k("a|b\r\nc\r\n", "x"), "|a\r\nc\r\n");
        assert_eq!(k("a|b\r\nc\r\n", "D"), "|a\r\nc\r\n");
        assert_eq!(k("|ab\r\nc\r\n", "ox<esc>"), "ab\r\n|x\r\nc\r\n");
        assert_eq!(k("|ab\r\nc\r\n", "Ox<esc>"), "|x\r\nab\r\nc\r\n");
        assert_eq!(k("|ab\r\nc\r\n", "J"), "ab| c\r\n");
        assert_eq!(k("ab\r\n|c\r\n", "dd"), "|ab\r\n");
    }

    #[test]
    fn de_keeps_final_newline() {
        assert_eq!(k("fo|o\n", "de"), "f|o\n");
        assert_eq!(run("a |foo\n", "ye").clip.as_deref(), Some("foo"));
    }

    #[test]
    fn visual_anchor_survives_reload() {
        let mut v = Vim::default();
        let long = Rope::from("abcdefghij\n");
        v.key("$", &long, 0, None);
        v.key("v", &long, 9, None);
        // The file is replaced with shorter, multi-byte text while the selection is up.
        let short = Rope::from("ææ\n");
        let out = v.key("d", &short, 0, None);
        let (r, _) = out.edit.expect("delete");
        assert!(r.end <= short.len() && short.is_char_boundary(r.start));
    }

    #[test]
    fn pending_display() {
        assert_eq!(run("|a", "2d").vim.pending(), "2d");
        assert_eq!(run("|a", "2d3").vim.pending(), "2d3");
        assert_eq!(run("|a", "g").vim.pending(), "g");
        assert_eq!(run("|a", "2d<esc>").vim.pending(), "");
        assert_eq!(run("|a", "dd").vim.pending(), "");
    }

    #[test]
    fn empty_buffer_never_panics() {
        let keys = [
            "h", "j", "k", "l", "w", "b", "e", "W", "B", "E", "0", "^", "$", "gg", "G", "{", "}",
            "x", "X", "dd", "yy", "cc<esc>", "D", "C<esc>", "p", "P", "J", "rX", "~", "dw",
            "cw<esc>", "de", "db", "dj", "dk", "dG", "dgg", "v", "vd", "V", "Vd", "vy", "V>", ">>",
            "<<", "o<esc>", "O<esc>", "i<esc>", "a<esc>", "A<esc>", "I<esc>", ":w<cr>", "5x",
            "3dd", "s<esc>", "S<esc>", "u", "<C-r>", "<C-d>", "/", "n",
        ];
        for k in keys {
            for src in ["|", "|\n", "\n|", "|\n\n"] {
                run_with(src, k, Some("z\n"));
                run_with(src, k, Some("z"));
            }
        }
    }

    #[test]
    fn trailing_newline() {
        assert_eq!(k("|a\nb\n", "j"), "a\n|b\n");
        assert_eq!(k("|a\nb\n", "jj"), "a\n|b\n");
        assert_eq!(k("|a\nb\n", "5j"), "a\n|b\n");
        assert_eq!(k("|a\nb\n", "G"), "a\n|b\n");
        assert_eq!(k("|a\nb\n", "9gg"), "a\n|b\n");
        assert_eq!(k("|a\nb\n", "}"), "a\n|b\n");
        assert_eq!(k("|a\nb\n", "w"), "a\n|b\n");
        assert_eq!(k("|a\nb\n", "dG"), "|");
        assert_eq!(k("a\n|b\n", "dd"), "|a\n");
        assert_eq!(k("a\n|b\n", "dj"), "a\n|b\n");
        assert_eq!(k("|a\nb\n", "dj"), "|");
        assert_eq!(k("|a\n", "dd"), "|");
        assert_eq!(k("|\n", "dd"), "|");
        assert_eq!(k("|a\n", "J"), "|a\n");
        assert_eq!(k("a\n|b\n", "Vjd"), "|a\n");
        assert_eq!(run("a\n|b\n", "Vj").last.select, Some(2..4));
        assert_eq!(run_with("a\n|b\n", "p", Some("X\n")).text, "a\nb\n|X\n");
        assert_eq!(k("a\n|b\n", "oX<esc>"), "a\nb\n|X\n");
        // Escape from insert on the pseudo-line clamps to the last real line.
        let mut v = Vim {
            mode: Mode::Insert,
            ..Vim::default()
        };
        assert_eq!(
            v.key("escape", &Rope::from("ab\n"), 3, None).cursor,
            Some(1)
        );
        // Incoming cursor on the pseudo-line is clamped before the key.
        assert_eq!(k("ab\n|", "x"), "|a\n");
    }

    #[test]
    fn public_clamp() {
        let v = Vim::default();
        let t = Rope::from("ab\n\ncd\n");
        assert_eq!(v.clamp(&t, 0), 0);
        assert_eq!(v.clamp(&t, 2), 1); // on '\n' -> last char
        assert_eq!(v.clamp(&t, 3), 3); // empty line
        assert_eq!(v.clamp(&t, 7), 5); // pseudo-line -> last real line
        assert_eq!(v.clamp(&t, 99), 5);
        let t = Rope::from("é😀");
        assert_eq!(v.clamp(&t, 3), 2); // mid-char -> char start
        assert_eq!(v.clamp(&t, 6), 2);
        for s in ["", "\n", "\n\n"] {
            let t = Rope::from(s);
            for c in 0..=s.len() + 1 {
                v.clamp(&t, c);
            }
        }
        assert_eq!(v.clamp(&Rope::from("\n"), 1), 0);
        let v = Vim {
            mode: Mode::Insert,
            ..Vim::default()
        };
        assert_eq!(v.clamp(&Rope::from("ab\n"), 3), 3);
    }

    #[test]
    fn perf() {
        use std::time::Instant;
        let mut s = String::new();
        for i in 0..50_000 {
            if i % 8 == 7 {
                s.push('\n');
            } else {
                s.push_str(&format!(
                    "    let value_{i} = some_function(arg, other.field); // é 😀 {i}\n"
                ));
            }
        }
        let mut rope = Rope::from(s.as_str());
        assert!(rope.len() > 2_500_000, "{}", rope.len());
        let mut cur = rope.len() / 2;
        let mut vim = Vim::default();
        let seq = [
            "j", "k", "w", "b", "e", "$", "0", "}", "{", "j", "x", "w", "d", "d", "k", "e",
        ];
        let start = Instant::now();
        for i in 0..20_000 {
            let out = vim.key(seq[i % seq.len()], &rope, cur, None);
            if let Some((r, t)) = out.edit {
                rope.remove(r.clone());
                rope.insert(r.start, &t);
            }
            if let Some(c) = out.cursor {
                cur = c;
            }
        }
        let el = start.elapsed();
        println!("vim perf: 20000 keys on {} bytes in {el:?}", rope.len());
        assert!(el.as_millis() < 300, "too slow: {el:?}");
    }

    /// The editor reports its caret at the selection end; visual mode must not drift with it.
    #[test]
    fn visual_with_editor_caret() {
        let e = |src: &str, keys: &str| {
            let r = run_caret(src, keys, None, true);
            (r.text, r.clip)
        };
        assert_eq!(e("a|bcd", "vd").0, "a|cd");
        assert_eq!(e("|abcdef", "vld").0, "|cdef");
        assert_eq!(e("|abcdef", "vlly").1.as_deref(), Some("abc"));
        assert_eq!(e("abc|def", "vhhd").0, "a|ef");
        assert_eq!(e("a\n|b\nc\n", "Vjd").0, "|a\n");
    }
}
