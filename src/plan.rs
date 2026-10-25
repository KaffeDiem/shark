//! Plan mode: the plans a chat's agent submits with `ExitPlanMode`, one version each, and the
//! pane that shows a version as markdown with its changes from the one before redlined in.
use crate::state::support_dir;
use crate::*;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::text::TextViewState;
use gpui_kit::component::{
    ActiveTheme, Icon, Selectable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde::{Deserialize, Serialize};
use similar::{Algorithm, DiffTag, capture_diff_slices};
use std::ops::Range;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Version {
    /// The `ExitPlanMode` tool call that submitted it, so its transcript row finds it.
    pub id: String,
    pub text: String,
    /// Later calls that resubmitted the same text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resubmits: Vec<String>,
}

/// Saved next to the transcripts, since the transcript clips tool inputs.
#[derive(Serialize, Deserialize, Default)]
struct Store {
    versions: Vec<Version>,
    /// The version the user approved, by index.
    approved: Option<usize>,
}

fn store_path(chat: &str) -> PathBuf {
    support_dir().join("plans").join(format!("{chat}.json"))
}

/// With the chat's transcript: a later chat with the same id starts with no plans.
pub fn delete(chat: &str) {
    let _ = std::fs::remove_file(store_path(chat));
}

#[cfg(test)]
pub fn tests_store_path(chat: &str) -> PathBuf {
    store_path(chat)
}

/// The plan's first line without its `#`, for the transcript row.
pub fn title(plan: &str) -> String {
    let line = plan
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default();
    line.trim()
        .trim_start_matches('#')
        .trim()
        .chars()
        .take(120)
        .collect()
}

/// A plan version rendered with what changed since the previous one marked inline.
#[derive(Debug, PartialEq)]
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
    let mut r = Redline {
        md: String::new(),
        hunks: vec![],
        added: 0,
        removed: 0,
    };
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

/// Lines, except that a fenced code block is one unit: its lines can't carry marks.
fn units(s: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut fence: Option<(usize, &str)> = None;
    let mut pos = 0;
    for line in s.split_inclusive('\n') {
        let t = line.trim();
        match fence {
            None if t.starts_with("```") || t.starts_with("~~~") => fence = Some((pos, &t[..3])),
            None => out.push(line),
            Some((start, marker))
                if t.starts_with(marker) && t.trim_matches(|c| c == '`' || c == '~').is_empty() =>
            {
                out.push(&s[start..pos + line.len()]);
                fence = None;
            }
            Some(_) => {}
        }
        pos += line.len();
    }
    if let Some((start, _)) = fence {
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
            // [text](url), else just the bracket's word
            match (rest.find("]("), rest.find(')')) {
                (Some(m), Some(e)) if m < e && !rest[..m].contains('\n') => e + 1,
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
            let (ta, tb) = (tokens(old), tokens(new));
            r.removed += words(&ta);
            r.added += words(&tb);
            return fence_diff(old, new);
        }
        let (old, new) = (old.trim_end_matches('\n'), new.trim_end_matches('\n'));
        if is_table(old) && is_table(new) && !is_rule(new) {
            let (ca, cb) = (cells(old), cells(new));
            if ca.len() == cb.len() {
                let row: Vec<String> = (ca.iter().zip(&cb))
                    .map(|(a, b)| {
                        self.inline(a, b, r).unwrap_or_else(|| {
                            r.removed += words(&tokens(a));
                            r.added += words(&tokens(b));
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
        r.removed += words(&tokens(a));
        r.added += words(&tokens(b));
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

/// `#rrggbb` of `color` at `alpha` over `bg`, for `<mark color>`, which takes no theme token.
fn tint(bg: Hsla, color: Hsla, alpha: f32) -> String {
    let c: Rgba = bg.blend(color.opacity(alpha)).into();
    let b = |v: f32| (v * 255.).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c.r), b(c.g), b(c.b))
}

/// The plan pane, next to the chat's transcript.
pub struct PlanPane {
    chat: String,
    pub versions: Vec<Version>,
    pub approved: Option<usize>,
    /// The version on screen.
    pub shown: usize,
    /// Whether the changes from the version before are marked.
    pub changes: bool,
    md: Entity<TextViewState>,
    hunks: Vec<Range<usize>>,
    hunk: Option<usize>,
    counts: (usize, usize),
    /// The marks' colors the text was built with; a theme switch rebuilds it.
    marks: (String, String),
    pub focus: FocusHandle,
}

impl PlanPane {
    pub fn new(chat: &str, cx: &mut Context<Self>) -> Self {
        let store: Store = std::fs::read(store_path(chat))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let mut p = PlanPane {
            chat: chat.into(),
            shown: store.versions.len().saturating_sub(1),
            versions: store.versions,
            approved: store.approved,
            changes: true,
            md: cx.new(|cx| TextViewState::markdown("", cx).scrollable(true)),
            hunks: vec![],
            hunk: None,
            counts: (0, 0),
            marks: Default::default(),
            focus: cx.focus_handle(),
        };
        p.rebuild(cx);
        p
    }

    fn save(&self) {
        let store = Store {
            versions: self.versions.clone(),
            approved: self.approved,
        };
        let path = store_path(&self.chat);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Written aside and renamed, so a failed write never truncates the history.
        let tmp = path.with_extension("json.tmp");
        if let Ok(json) = serde_json::to_vec(&store)
            && std::fs::write(&tmp, json).is_ok()
        {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    /// Adds a submitted plan and shows it with its changes. The same text again is the same
    /// version. Returns its number, from 1.
    pub fn add(&mut self, id: String, text: String, cx: &mut Context<Self>) -> usize {
        match self.versions.last_mut() {
            Some(v) if v.text == text => v.resubmits.push(id),
            _ => self.versions.push(Version {
                id,
                text,
                resubmits: vec![],
            }),
        }
        self.save();
        self.shown = self.versions.len() - 1;
        self.changes = true;
        self.rebuild(cx);
        self.versions.len()
    }

    /// The version number a transcript row's tool call submitted.
    pub fn number_of(&self, id: &str) -> Option<usize> {
        (self.versions.iter())
            .position(|v| v.id == id || v.resubmits.iter().any(|r| r == id))
            .map(|i| i + 1)
    }

    pub fn latest(&self) -> Option<&Version> {
        self.versions.last()
    }

    pub fn approve(&mut self, cx: &mut Context<Self>) {
        self.approved = self.versions.len().checked_sub(1);
        self.save();
        cx.notify();
    }

    /// `y`: the shown version's markdown, without the change marks.
    pub fn copy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(v) = self.versions.get(self.shown) {
            cx.write_to_clipboard(ClipboardItem::new_string(v.text.clone()));
            window.push_notification("Copied the plan", cx);
        }
    }

    pub fn show(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.versions.len() && ix != self.shown {
            self.shown = ix;
            self.rebuild(cx);
        }
    }

    fn toggle_changes(&mut self, cx: &mut Context<Self>) {
        self.changes = !self.changes;
        self.rebuild(cx);
    }

    fn colors(cx: &App) -> (String, String) {
        let t = cx.theme();
        (
            tint(t.background, t.success, 0.18),
            tint(t.background, t.danger, 0.14),
        )
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.marks = Self::colors(cx);
        let Some(new) = self.versions.get(self.shown) else {
            return;
        };
        let r = match (self.changes, self.shown.checked_sub(1)) {
            (true, Some(prev)) => redline(
                &self.versions[prev].text,
                &new.text,
                &self.marks.0,
                &self.marks.1,
            ),
            _ => Redline {
                md: new.text.clone(),
                hunks: vec![],
                added: 0,
                removed: 0,
            },
        };
        self.hunks = r.hunks;
        self.hunk = None;
        self.counts = (r.added, r.removed);
        self.md.update(cx, |m, cx| m.set_text(&r.md, cx));
        cx.notify();
    }

    /// `]c` / `[c`: scrolls the next or previous change into view.
    fn jump(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.hunks.is_empty() {
            return;
        }
        let n = self.hunks.len() as isize;
        let at = match self.hunk {
            Some(h) => (h as isize + delta).clamp(0, n - 1),
            None if delta > 0 => 0,
            None => n - 1,
        } as usize;
        self.hunk = Some(at);
        let src = self.hunks[at].clone();
        self.md.update(cx, |m, cx| {
            if let Some(range) = m.rendered_text().range_for_source(src) {
                let _ = m.reveal_range(range.start..range.start, cx);
            }
        });
        cx.notify();
    }

    fn scroll(&self, lines: f32, cx: &mut Context<Self>) {
        self.md.read(cx).list_state().scroll_by(px(lines * 21.));
        cx.notify();
    }
}

impl Render for PlanPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.marks != Self::colors(cx) {
            self.rebuild(cx);
        }
        let theme = cx.theme();
        let focused = self.focus.is_focused(window);
        let n = self.versions.len();
        let latest = self.shown + 1 == n;
        let (muted, faint) = (theme.muted_foreground, crate::app::faint(theme));
        let pill = |i: usize, theme: &gpui_kit::component::Theme| {
            let active = i == self.shown;
            h_flex()
                .id(("plan-version", i))
                .h(px(22.))
                .px_2()
                .gap_1()
                .rounded_full()
                .text_xs()
                .when(active, |d| {
                    d.bg(theme.secondary).text_color(theme.foreground)
                })
                .when(!active, |d| {
                    d.text_color(theme.muted_foreground)
                        .hover(|d| d.text_color(theme.foreground))
                })
                .when(self.approved == Some(i), |d| {
                    d.child(
                        Icon::new(IconName::Check)
                            .with_size(px(12.))
                            .text_color(theme.success_foreground),
                    )
                })
                .child(format!("v{}", i + 1))
                .on_click(cx.listener(move |this, _, _, cx| this.show(i, cx)))
        };
        let header = h_flex()
            .h(px(40.))
            .flex_none()
            .px_3()
            .gap_1()
            .border_b_1()
            .border_color(if focused { theme.primary } else { theme.border })
            .child(
                Icon::new(IconName::ClipboardList)
                    .with_size(px(14.))
                    .text_color(muted),
            )
            .child(
                div()
                    .pl_1()
                    .pr_2()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child("Plan"),
            )
            .when(n > 1, |d| d.children((0..n).map(|i| pill(i, theme))))
            .child(div().flex_1())
            .when(self.shown > 0 && self.changes, |d| {
                d.child(
                    h_flex()
                        .gap_1()
                        .pr_1()
                        .text_xs()
                        .child(
                            div()
                                .text_color(theme.success_foreground)
                                .child(format!("+{}", self.counts.0)),
                        )
                        .child(
                            div()
                                .text_color(theme.danger_foreground)
                                .child(format!("−{}", self.counts.1)),
                        ),
                )
            })
            .when(self.shown > 0, |d| {
                d.child(
                    Button::new("plan-changes")
                        .ghost()
                        .xsmall()
                        .icon(IconName::GitCompare)
                        .label("Changes")
                        .selected(self.changes)
                        .tooltip(format!("Mark changes from v{} (d)", self.shown))
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_changes(cx))),
                )
            })
            .when(n > 0, |d| {
                d.child(
                    Clipboard::new("plan-copy")
                        .value(self.versions[self.shown].text.clone())
                        .tooltip("Copy plan (y)"),
                )
            })
            .child(
                Button::new("plan-close")
                    .ghost()
                    .xsmall()
                    .icon(IconName::X)
                    .tooltip("Hide plan")
                    .on_click(|_, window, cx| window.dispatch_action(TogglePlan.boxed_clone(), cx)),
            );
        let body = match n {
            0 => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_3()
                .px_6()
                .child(Icon::new(IconName::ClipboardList).with_size(px(32.)).text_color(faint))
                .child(div().text_base().child("No plan yet"))
                .child(
                    div()
                        .max_w(px(320.))
                        .text_sm()
                        .text_center()
                        .text_color(muted)
                        .child("The agent's plan shows up here. Reply in the chat to revise it; each revision is a new version, with its changes marked."),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .text_color(muted)
                        .child(Kbd::new(Keystroke::parse("shift-tab").unwrap()))
                        .child("plan mode on/off"),
                )
                .into_any_element(),
            _ => div()
                .group("plan-body")
                .flex_1()
                .min_h_0()
                .px_4()
                .pt_2()
                .text_size(px(14.))
                .line_height(relative(1.5))
                .child(crate::chat::markdown(&self.md, "plan-body").scrollable(true))
                .into_any_element(),
        };
        let hint = |keys: &[&str], label: &'static str| {
            h_flex()
                .gap_1()
                .children(keys.iter().map(|k| Kbd::new(Keystroke::parse(k).unwrap())))
                .child(label)
        };
        let footer = h_flex()
            .flex_none()
            .h(px(44.))
            .px_3()
            .gap_3()
            .border_t_1()
            .border_color(theme.border)
            .text_xs()
            .text_color(muted)
            .when(n > 1, |d| d.child(hint(&["h", "l"], "versions")))
            .when(!self.hunks.is_empty(), |d| {
                d.child(hint(&["]", "c"], "next change"))
            })
            .child(div().flex_1())
            .when(n > 0 && !latest, |d| {
                d.child(format!("Viewing v{} of {n}", self.shown + 1))
            })
            .when(n > 0 && latest && self.approved == Some(self.shown), |d| {
                d.child(
                    h_flex()
                        .gap_1()
                        .child(
                            Icon::new(IconName::Check)
                                .with_size(px(12.))
                                .text_color(theme.success_foreground),
                        )
                        .child("Approved"),
                )
            })
            .when(n > 0 && latest && self.approved != Some(self.shown), |d| {
                d.child(
                    Button::new("plan-new-chat")
                        .ghost()
                        .small()
                        .icon(IconName::Plus)
                        .label("Build in new chat")
                        .tooltip("Implement this plan in a new chat that gets only the plan (n)")
                        .on_click(|_, window, cx| {
                            window.dispatch_action(BuildPlanInNewChat.boxed_clone(), cx)
                        }),
                )
                .child(
                    Button::new("plan-approve")
                        .primary()
                        .small()
                        .icon(IconName::Hammer)
                        .label("Approve and build")
                        .tooltip("Leave plan mode and implement this plan (a)")
                        .on_click(|_, window, cx| {
                            window.dispatch_action(ApprovePlan.boxed_clone(), cx)
                        }),
                )
            });
        v_flex()
            .id("plan-pane")
            .key_context("PlanPane")
            .track_focus(&self.focus)
            .size_full()
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.scroll(-2., cx)))
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.scroll(2., cx)))
            .on_action(cx.listener(|this, _: &PageDown, _, cx| this.scroll(-15., cx)))
            .on_action(cx.listener(|this, _: &PageUp, _, cx| this.scroll(15., cx)))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| {
                this.md
                    .read(cx)
                    .list_state()
                    .scroll_to(ListOffset::default());
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| {
                this.md.read(cx).list_state().scroll_to_end();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &NextHunk, _, cx| this.jump(1, cx)))
            .on_action(cx.listener(|this, _: &PrevHunk, _, cx| this.jump(-1, cx)))
            .on_action(
                cx.listener(|this, _: &PlanOlder, _, cx| {
                    this.show(this.shown.saturating_sub(1), cx)
                }),
            )
            .on_action(cx.listener(|this, _: &PlanNewer, _, cx| this.show(this.shown + 1, cx)))
            .on_action(cx.listener(|this, _: &TogglePlanChanges, _, cx| this.toggle_changes(cx)))
            .on_action(cx.listener(|this, _: &CopySelected, w, cx| this.copy(w, cx)))
            .on_click(cx.listener(|this, _, window, cx| this.focus.focus(window, cx)))
            .child(header)
            .child(body)
            .when(n > 0, |d| d.child(footer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin

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
        assert_eq!(title("\n# Add a cache\n\nmore"), "Add a cache");
    }
}
