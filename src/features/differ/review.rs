//! Diff review: a keyboard line cursor that survives refreshes, and the review queue: line
//! comments that stay pending until sent as one prompt (⇧C, review_send.rs), then show what
//! the agent made of them (Addressed / Outdated), and can go to GitHub (review_share.rs).
//!
//! gpui-kit's `DiffState` keeps its scroll offset private and `set_files` resets it, so the
//! keyboard view is tracked here instead: `j/k` move a cursor over a model of the diff's rows
//! and scroll by putting a row at the top with `scroll_to_line`. A refresh puts the same row
//! back. Mouse scrolling loses the model; `j/k` then scroll natively until the next jump.
use super::*;
use crate::chat::parse_reports;
use crate::diff_view::{DiffAnnotation, DiffLinePosition, DiffLineRange, DiffSide};
use crate::state::support_dir;
use gpui_kit::component::input::{Enter, Escape, Textarea, TextareaState};
use serde::{Deserialize, Serialize};
use std::cell::Cell;

/// One hunk's patch lines: kind (`b' '`, `b'-'`, `b'+'`), old and new one-based line numbers.
pub(crate) type Hunk = Vec<(u8, usize, usize)>;

/// Hunks of one file's patch chunk, so the cursor knows the rows the diff draws.
pub(crate) fn parse_hunks(chunk: &str) -> Vec<Hunk> {
    let mut hunks: Vec<Hunk> = vec![];
    let (mut old, mut new, mut inside) = (0, 0, false);
    for line in chunk.lines() {
        if let Some(rest) = line.strip_prefix("@@ -") {
            let start = |s: &str| s.split(',').next()?.parse::<usize>().ok();
            let mut parts = rest.split(' ');
            let o = parts.next().and_then(start);
            let n = parts.next().and_then(|s| start(s.strip_prefix('+')?));
            inside = o.is_some() && n.is_some();
            (old, new) = (o.unwrap_or(0), n.unwrap_or(0));
            if inside {
                hunks.push(vec![]);
            }
            continue;
        }
        let Some(h) = hunks.last_mut().filter(|_| inside) else {
            continue;
        };
        match line.as_bytes().first() {
            Some(b' ') | None => {
                h.push((b' ', old, new));
                (old, new) = (old + 1, new + 1);
            }
            Some(b'-') => {
                h.push((b'-', old, 0));
                old += 1;
            }
            Some(b'+') => {
                h.push((b'+', 0, new));
                new += 1;
            }
            Some(b'\\') => {}
            _ => inside = false,
        }
    }
    hunks
}

/// A row the cursor can sit on: a file header or a source line.
#[derive(Clone, Debug, PartialEq)]
enum Spot {
    File(SharedString),
    Line(DiffLinePosition),
}

impl Spot {
    fn path(&self) -> &SharedString {
        match self {
            Spot::File(p) => p,
            Spot::Line(l) => l.path(),
        }
    }
}

#[derive(Debug)]
struct Row {
    spot: Spot,
    /// The original line also shown on this row: a context line, or a split pair's removal.
    old: Option<usize>,
    changed: bool,
    /// Height in code rows; headers and hunk separators are taller than a line.
    h: f32,
}

/// The diff's rows in display order, like `DiffState` projects them: unified mode lists a
/// change's removed lines before its added ones, split mode pairs them. Collapsed (viewed)
/// files are a header only.
// ponytail: rebuilt per key, ~1ms per 50k patch lines; cache per refresh if it shows up.
fn rows(files: &[(SharedString, &[Hunk], bool)], split: bool) -> Vec<Row> {
    let mut out = vec![];
    for (path, hunks, collapsed) in files {
        out.push(Row {
            spot: Spot::File(path.clone()),
            old: None,
            changed: false,
            h: 2.,
        });
        if *collapsed {
            continue;
        }
        let line =
            |old: bool, n: usize| Spot::Line(DiffLinePosition::new(path.clone(), side(old), n));
        for hunk in hunks.iter() {
            let first = out.len();
            let mut i = 0;
            while i < hunk.len() {
                if hunk[i].0 == b' ' {
                    out.push(Row {
                        spot: line(false, hunk[i].2),
                        old: Some(hunk[i].1),
                        changed: false,
                        h: 1.,
                    });
                    i += 1;
                    continue;
                }
                let end = (i..hunk.len())
                    .find(|&j| hunk[j].0 == b' ')
                    .unwrap_or(hunk.len());
                let (dels, adds): (Vec<usize>, Vec<usize>) =
                    (i..end).partition(|&j| hunk[j].0 == b'-');
                let spots: Vec<(Spot, Option<usize>)> = match split {
                    false => (dels.iter().map(|&j| (line(true, hunk[j].1), None)))
                        .chain(adds.iter().map(|&j| (line(false, hunk[j].2), None)))
                        .collect(),
                    true => (0..dels.len().max(adds.len()))
                        .map(|k| {
                            let old = dels.get(k).map(|&j| hunk[j].1);
                            match adds.get(k) {
                                Some(&j) => (line(false, hunk[j].2), old),
                                None => (line(true, hunk[dels[k]].1), None),
                            }
                        })
                        .collect(),
                };
                out.extend(spots.into_iter().map(|(spot, old)| Row {
                    spot,
                    old,
                    changed: true,
                    h: 1.,
                }));
                i = end;
            }
            if let Some(r) = out.get_mut(first) {
                r.h += 1.; // the hunk separator above it
            }
        }
    }
    out
}

/// The row showing `spot`, or the closest one before it in the same file after a refresh.
fn find(rows: &[Row], spot: &Spot) -> usize {
    // An original line is on its own row, or beside the working line it pairs with.
    let before = |r: &Row, want: &DiffLinePosition| match &r.spot {
        Spot::Line(l) if l.side() == want.side() => l.line() <= want.line(),
        Spot::Line(_) if want.side() == DiffSide::Original => {
            r.old.is_some_and(|o| o <= want.line())
        }
        _ => false,
    };
    if let Some(i) = rows.iter().position(|r| {
        r.spot == *spot
            || matches!(spot, Spot::Line(w) if w.side() == DiffSide::Original
                && r.spot.path() == w.path() && r.old == Some(w.line()))
    }) {
        return i;
    }
    let Some(head) = rows
        .iter()
        .position(|r| r.spot == Spot::File(spot.path().clone()))
    else {
        return 0;
    };
    let Spot::Line(want) = spot else {
        return head;
    };
    let same_file = rows[head + 1..]
        .iter()
        .take_while(|r| matches!(&r.spot, Spot::Line(_)));
    same_file
        .enumerate()
        .filter(|(_, r)| before(r, want))
        .last()
        .map_or(head, |(i, _)| head + 1 + i)
}

const SCROLLOFF: f32 = 3.;

/// The top row that keeps `cur` on screen, `cap` code rows high, with a margin around it.
fn fit(rows: &[Row], top: usize, cur: usize, cap: f32) -> usize {
    let mut top = top.min(cur.saturating_sub(SCROLLOFF as usize));
    while top < cur && rows[top..=cur].iter().map(|r| r.h).sum::<f32>() + SCROLLOFF > cap {
        top += 1;
    }
    // Past the end the list stops scrolling; stay where it really is so `k` moves at once.
    let mut last = rows.len();
    let mut tail = 0.;
    while last > 0 && tail + rows[last - 1].h <= cap {
        last -= 1;
        tail += rows[last].h;
    }
    top.min(last)
}

/// Index of the next (or previous) start of a run of changed rows, wrapping around.
fn next_change(rows: &[Row], cur: usize, forward: bool) -> Option<usize> {
    let starts: Vec<usize> = (0..rows.len())
        .filter(|&i| rows[i].changed && (i == 0 || !rows[i - 1].changed))
        .collect();
    match forward {
        true => starts.iter().find(|&&i| i > cur).or(starts.first()),
        false => starts.iter().rev().find(|&&i| i < cur).or(starts.last()),
    }
    .copied()
}

/// Where a thread stands. Files from before statuses load as `Pending`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub(crate) enum Status {
    /// Waiting for the next send (⇧C).
    #[default]
    Pending,
    /// Waiting in `Comment::chat`'s queue.
    Queued,
    /// In `Comment::chat`, read in this turn.
    Sent { turn: usize },
    /// The agent's reply reported it done (`[c<id>] addressed: <note>`).
    Addressed { turn: usize, note: String },
    /// Its lines changed and no report came.
    Outdated,
    /// Hidden; kept in the store.
    Resolved,
}

impl Status {
    /// Resolvable once sent; a pending or queued thread would vanish unsent.
    fn resolvable(&self) -> bool {
        matches!(
            self,
            Status::Sent { .. } | Status::Addressed { .. } | Status::Outdated
        )
    }
}

/// A comment on lines of the diff: pending until sent, then followed to an answer.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub(crate) struct Comment {
    #[serde(default)]
    pub(crate) id: u64,
    pub(crate) path: String,
    /// `(on the old side, one-based line)` of the first and last commented line.
    start: (bool, usize),
    end: (bool, usize),
    pub(crate) text: String,
    /// The commented lines when it was written; kept when they change or go.
    #[serde(default)]
    pub(crate) snapshot: String,
    /// Code proposed to replace them.
    #[serde(default)]
    pub(crate) suggestion: Option<String>,
    /// What the lines were compared at: the base commit for the old side, else "working tree".
    #[serde(default)]
    pub(crate) rev: String,
    #[serde(default)]
    pub(crate) status: Status,
    /// The chat it was sent to.
    #[serde(default)]
    pub(crate) chat: Option<String>,
    /// Posted to GitHub: the comment's (or review's) URL.
    #[serde(default)]
    pub(crate) github: Option<String>,
    /// The file text its working-copy lines were last read in.
    #[serde(skip)]
    seen: Seen,
}

/// Memory only, and not part of a comment's equality (that decides saving).
#[derive(Clone, Debug, Default)]
struct Seen(Option<String>);

impl PartialEq for Seen {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Comment {
    fn from_range(r: &DiffLineRange) -> Self {
        let old = |s: DiffSide| s == DiffSide::Original;
        Comment {
            path: r.path().to_string(),
            start: (old(r.side()), r.start()),
            end: (old(r.end_side()), r.end()),
            ..Default::default()
        }
    }

    /// `api/a.rs:12`, `api/a.rs:12-14`, `api/a.rs:7 (old)`; across sides `7 (old)-9 (new)`.
    pub(crate) fn place(&self) -> String {
        let n = |(old, line): (bool, usize)| match old {
            true => format!("{line} (old)"),
            false => line.to_string(),
        };
        match self.start == self.end {
            true => format!("{}:{}", self.path, n(self.start)),
            false if self.start.0 == self.end.0 => {
                format!("{}:{}-{}", self.path, self.start.1, n(self.end))
            }
            false => {
                let n = |(old, line)| format!("{line} ({})", if old { "old" } else { "new" });
                format!("{}:{}-{}", self.path, n(self.start), n(self.end))
            }
        }
    }

    /// The repo its path starts with.
    pub(crate) fn repo(&self) -> &str {
        self.path.split('/').next().unwrap_or_default()
    }

    /// `631–632`, `12`.
    pub(crate) fn lines(&self) -> String {
        match self.start.1 == self.end.1 {
            true => self.end.1.to_string(),
            false => format!("{}–{}", self.start.1, self.end.1),
        }
    }

    pub(crate) fn side(&self) -> &'static str {
        if self.end.0 {
            "original"
        } else {
            "working copy"
        }
    }

    /// `app.rs · Working copy · lines 631–632`, as the chat card and GitHub preview say it.
    pub(crate) fn card_place(&self) -> String {
        let name = self.path.rsplit('/').next().unwrap_or(&self.path);
        let side = if self.end.0 {
            "Original"
        } else {
            "Working copy"
        };
        let s = crate::chat::plural(self.start.1.abs_diff(self.end.1) + 1);
        format!("{name} · {side} · line{s} {}", self.lines())
    }

    /// Its lines in the PR, for an inline review comment; `None` across both sides.
    pub(crate) fn gh_anchor(&self) -> Option<crate::pr::Anchor> {
        let (_, rel) = self.path.split_once('/')?;
        (self.start.0 == self.end.0).then(|| crate::pr::Anchor {
            path: rel.to_string(),
            line: self.end.1,
            start: (self.start.1 < self.end.1).then_some(self.start.1),
            left: self.end.0,
        })
    }

    fn anchor(&self) -> DiffLinePosition {
        DiffLinePosition::new(self.path.clone(), side(self.end.0), self.end.1)
    }

    fn shown(&self) -> bool {
        self.status != Status::Resolved
    }

    /// Whether line `l` of the diff falls inside this comment.
    fn covers(&self, l: &DiffLinePosition) -> bool {
        l.path().as_str() == self.path
            && side(self.end.0) == l.side()
            && (if self.start.0 == self.end.0 {
                self.start.1
            } else {
                self.end.1
            }..=self.end.1)
                .contains(&l.line())
    }
}

fn side(old: bool) -> DiffSide {
    if old {
        DiffSide::Original
    } else {
        DiffSide::Modified
    }
}

/// `s` as a code block (`info` after the opening fence), its fence longer than any backtick
/// run in it.
pub(crate) fn fenced(s: &str, info: &str) -> String {
    let mut f = "```".to_string();
    while s.contains(&f) {
        f.push('`');
    }
    format!("{f}{info}\n{s}\n{f}")
}

/// The comments as one prompt, in file order, with the overall `note`. Each carries its id,
/// revision and the lines as they were, and the agent is asked to report back per id.
pub(crate) fn prompt(comments: &[Comment], note: &str) -> String {
    let mut out = String::from(
        "Review comments on the diff. Paths are workspace-relative; \"(old)\" is a line number \
         in the base version (a removed line). Each comment quotes the lines as they were when \
         it was written. Address each comment:\n",
    );
    for c in comments {
        let rev = match c.rev.as_str() {
            "" => String::new(),
            "working tree" => " · working tree".into(),
            r => format!(" · base {}", &r[..r.len().min(10)]),
        };
        out.push_str(&format!("\n[c{}] {}{rev}\n", c.id, c.place()));
        if !c.snapshot.is_empty() {
            out.push_str(&format!("{}\n", fenced(&c.snapshot, "")));
        }
        for l in c.text.lines() {
            out.push_str(&format!("> {l}\n"));
        }
        if let Some(s) = &c.suggestion {
            out.push_str(&format!("Suggested change:\n{}\n", fenced(s, "")));
        }
    }
    if !note.trim().is_empty() {
        out.push_str(&format!("\nOverall: {}\n", note.trim()));
    }
    out.push_str(
        "\nEnd your reply with one line per comment you addressed, exactly like:\n\
         [c<id>] addressed: <what you changed, in one line>\n",
    );
    out
}

/// A sent comment after turn `turn` ended: reported → Addressed; its lines no longer read like
/// the snapshot (`now`, `None` when unknown) → Outdated; else as it was.
pub(crate) fn judge(
    status: &Status,
    turn: usize,
    report: Option<&str>,
    now: Option<&str>,
    snapshot: &str,
) -> Status {
    match (status, report, now) {
        (Status::Sent { .. }, Some(note), _) => Status::Addressed {
            turn,
            note: note.to_string(),
        },
        (Status::Sent { .. }, None, Some(now)) if !snapshot.is_empty() && now != snapshot => {
            Status::Outdated
        }
        _ => status.clone(),
    }
}

/// Lines `start..=end` (one-based) of `src`.
fn slice(src: &str, start: usize, end: usize) -> String {
    (src.lines().skip(start.saturating_sub(1)))
        .take((end + 1).saturating_sub(start))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Moves a working-copy comment along with its lines from the text they were last read in to
/// `src`: only when they are unchanged and shifted. Edited or gone, it stays, so judging finds
/// them changed. Never read yet (loaded from disk), it goes to its snapshot only if `src` has
/// it once.
fn reanchor(c: &mut Comment, src: &str) {
    if c.start.0 || c.end.0 {
        return;
    }
    let (s, e) = (c.start.1.saturating_sub(1), c.end.1.saturating_sub(1));
    let Some(old) = c.seen.0.replace(src.to_string()) else {
        let want: Vec<&str> = c.snapshot.split('\n').collect();
        let lines: Vec<&str> = src.lines().collect();
        let mut found = (lines.windows(want.len()).enumerate())
            .filter(|(_, w)| !c.snapshot.is_empty() && *w == &want[..])
            .map(|(i, _)| i);
        if let (Some(i), None) = (found.next(), found.next()) {
            (c.start.1, c.end.1) = (i + 1, i + 1 + e - s);
        }
        return;
    };
    // ponytail: one copy of the file per thread; share per path if threads get many.
    let diff = similar::TextDiff::configure()
        .timeout(std::time::Duration::from_millis(30))
        .diff_lines(old.as_str(), src);
    let mut to = vec![None; diff.old_slices().len()];
    for op in diff.ops() {
        if let similar::DiffOp::Equal {
            old_index,
            new_index,
            len,
        } = *op
        {
            (0..len).for_each(|i| to[old_index + i] = Some(new_index + i));
        }
    }
    if let Some(&Some(n)) = to.get(s)
        && (s..=e).all(|i| to.get(i) == Some(&Some(n + i - s)))
    {
        (c.start.1, c.end.1) = (n + 1, n + 1 + e - s);
    }
}

/// The comment's lines as they are now: the working tree's on the new side (the comment
/// re-anchored to where they moved), the base commit's (`rev`) on the old one. A deleted file
/// reads empty; `None` for a range across both sides.
fn lines_now(dir: &Path, c: &mut Comment) -> Option<String> {
    if c.start.0 != c.end.0 {
        return None;
    }
    let src = match c.end.0 {
        false => {
            let src = std::fs::read_to_string(dir.join(&c.path)).unwrap_or_default();
            reanchor(c, &src);
            src
        }
        // No base known (merge-base failed): `git show :path` would read the index.
        true if c.rev.is_empty() => return None,
        true => {
            let (repo, rel) = c.path.split_once('/')?;
            let o = std::process::Command::new("git")
                .current_dir(dir.join(repo))
                .args(["show", &format!("{}:{rel}", c.rev)])
                .output()
                .ok()?;
            o.status
                .success()
                .then(|| String::from_utf8_lossy(&o.stdout).into_owned())?
        }
    };
    Some(slice(&src, c.start.1, c.end.1))
}

/// Fingerprint of one file's patch for its viewed mark: the working-tree blob the patch ends
/// in, so switching Uncommitted (another old side) alone doesn't mark it changed. Falls back
/// to the whole chunk when there is none (deleted, mode-only).
pub(crate) fn chunk_fingerprint(chunk: &str) -> u64 {
    let new = (chunk.lines().find_map(|l| l.strip_prefix("index ")))
        .and_then(|l| l.split_whitespace().next()?.split_once(".."))
        .map(|(_, new)| new)
        .filter(|new| new.bytes().any(|b| b != b'0'));
    crate::files::fingerprint(new.unwrap_or(chunk))
}

/// The commit the diff of `repo_dir` compares against: HEAD when only uncommitted changes show,
/// else the merge-base with `base`. Empty when git fails.
pub(crate) fn compare_rev(repo_dir: &Path, base: &str, uncommitted_only: bool) -> String {
    let args = match uncommitted_only {
        true => vec!["rev-parse", "HEAD"],
        false => vec!["merge-base", base, "HEAD"],
    };
    crate::git::git(repo_dir, &args).unwrap_or_default()
}

/// Ids for comments saved before there were any (they load as 0), counting on from `last`;
/// true if any got one.
fn give_ids(c: &mut [Comment], last: &mut u64) -> bool {
    *last = c.iter().map(|c| c.id).fold(*last, u64::max);
    let fresh = c.iter().any(|c| c.id == 0);
    for c in c.iter_mut().filter(|c| c.id == 0) {
        *last += 1;
        c.id = *last;
    }
    fresh
}

/// Comments that never reached an agent are pending again; true if any was. With `chat`
/// (closed): its queue and the reviews it hadn't read (`unread`). Without (loading, after a
/// restart): every queue, and reviews sent to a chat that is gone.
pub(crate) fn unqueue(l: &mut [Comment], chat: Option<&str>, unread: &[u64]) -> bool {
    let mut any = false;
    for c in l.iter_mut() {
        let to = c.chat.as_deref();
        let lost = match (&c.status, chat) {
            (Status::Queued, None) => true,
            (Status::Queued, Some(id)) => to == Some(id),
            (Status::Sent { .. }, Some(id)) => to == Some(id) && unread.contains(&c.id),
            (Status::Sent { .. }, None) => {
                to.is_some_and(|id| !crate::agent::transcript_path(id).exists())
            }
            _ => false,
        };
        if lost {
            (c.status, c.chat) = (Status::Pending, None);
            any = true;
        }
    }
    any
}

/// comments/<ws>.json: the comments and the last id given out, so a deleted comment's id
/// isn't reused (old review cards link to it). Older files are the list alone.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Store {
    Ids { last: u64, comments: Vec<Comment> },
    List(Vec<Comment>),
}

pub(crate) fn comments_path(ws: &str) -> PathBuf {
    support_dir().join("comments").join(format!("{ws}.json"))
}

/// The inline composer (A·01): under the commented lines, on their side.
pub(crate) struct Composer {
    /// The comment being edited; id 0 for a new one on its lines.
    pub(crate) draft: Comment,
    pub(crate) input: Entity<TextareaState>,
    /// Suggest change (cmd-shift-s): the lines, to edit into the proposed code.
    pub(crate) suggest: Option<Entity<TextareaState>>,
    /// Its height as last laid out; when it grows, the diff scrolls it into view.
    height: Rc<Cell<Pixels>>,
}

#[derive(Default)]
pub(crate) struct Review {
    /// Top row and cursor of the keyboard view; `None` once the mouse scrolled.
    view: Option<(Spot, Spot)>,
    pub(crate) hunks: HashMap<SharedString, Vec<Hunk>>,
    /// The diff pane as last painted.
    viewport: Rc<Cell<Bounds<Pixels>>>,
    /// Comments per workspace, loaded from disk on first use.
    pub(crate) comments: HashMap<String, Vec<Comment>>,
    /// The last comment id given out per workspace.
    last_id: HashMap<String, u64>,
    /// Workspaces whose comments file couldn't be read or moved aside: never saved over.
    unsaved: Vec<String>,
    pub(crate) failed_saves: HashMap<String, String>,
    /// Why, for the window to show once.
    pub(crate) warning: Option<String>,
    pub(crate) composer: Option<Composer>,
    pub(crate) sheet: Option<review_send::Sheet>,
    pub(crate) share: Option<review_share::Share>,
    /// The thread last jumped to, and the lines selected for it: while they stay selected it
    /// is the thread there, though another one starts on the same line.
    jumped: Option<(u64, DiffLineRange)>,
}

impl Review {
    /// `}`/`{` and the tree moved the diff to a file header.
    pub(crate) fn at_file(&mut self, path: &str) {
        let s = Spot::File(path.to_string().into());
        self.view = Some((s.clone(), s));
    }

    /// Writes the file whole (a temp file renamed over it), so a crash can't leave half.
    pub(crate) fn save_comments(&mut self, ws: &str) {
        let path = comments_path(ws);
        let result = if self.unsaved.iter().any(|w| w == ws) {
            Err(std::io::Error::other(
                "The unreadable comments store is protected",
            ))
        } else {
            serde_json::to_vec(&Store::Ids {
                last: self.last_id.get(ws).copied().unwrap_or(0),
                comments: self.comments.get(ws).cloned().unwrap_or_default(),
            })
            .map_err(std::io::Error::other)
            .and_then(|json| crate::state::write(&path, &json))
        };
        match result {
            Ok(()) => {
                self.failed_saves.remove(ws);
            }
            Err(e) => {
                self.failed_saves.insert(ws.to_string(), e.to_string());
            }
        }
    }

    pub(crate) fn retry_saves(&mut self) {
        for ws in self.failed_saves.keys().cloned().collect::<Vec<_>>() {
            self.save_comments(&ws);
        }
    }

    /// Loads once; comments saved before ids get their own, ones queued in a chat that is
    /// gone now are pending. A file that doesn't parse is kept aside as `.json.<n>.bak`; one
    /// that can't be read (or moved) is left alone and not saved over.
    pub(crate) fn load_comments(&mut self, ws: &str) {
        if self.comments.contains_key(ws) {
            return;
        }
        let path = comments_path(ws);
        let (store, read) = match std::fs::read(&path) {
            Ok(b) => (
                Some(serde_json::from_slice(&b).map_err(|e| e.to_string())),
                true,
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (None, true),
            Err(e) => (Some(Err(e.to_string())), false),
        };
        let (mut last, mut c) = match store {
            Some(Ok(Store::Ids { last, comments })) => (last, comments),
            Some(Ok(Store::List(c))) => (0, c),
            Some(Err(e)) => {
                let bak = (1..)
                    .map(|n| path.with_extension(format!("json.{n}.bak")))
                    .find(|p| !p.exists())
                    .unwrap();
                let file = path.display();
                self.warning = Some(match read && std::fs::rename(&path, &bak).is_ok() {
                    true => format!("Couldn't read {file} ({e}); moved it to {}", bak.display()),
                    false => {
                        self.unsaved.push(ws.to_string());
                        format!("Couldn't read {file} ({e}); nothing is saved until it's fixed")
                    }
                });
                (0, vec![])
            }
            None => (0, vec![]),
        };
        let fresh = give_ids(&mut c, &mut last) | unqueue(&mut c, None, &[]);
        self.comments.insert(ws.to_string(), c);
        self.last_id.insert(ws.to_string(), last);
        if fresh {
            self.save_comments(ws);
        }
    }

    /// A new comment id in workspace `ws`, never one given out before.
    fn new_id(&mut self, ws: &str) -> u64 {
        self.load_comments(ws);
        let last = self.last_id.entry(ws.to_string()).or_default();
        *last += 1;
        *last
    }

    /// A removed workspace's comments, so a later one of the same name starts with none.
    pub(crate) fn forget(&mut self, ws: &str) {
        self.comments.remove(ws);
        self.last_id.remove(ws);
        self.unsaved.retain(|w| w != ws);
        self.failed_saves.remove(ws);
        let _ = std::fs::remove_file(comments_path(ws));
    }

    /// Edits workspace `ws`'s comments and saves them if that changed them.
    pub(crate) fn edit(&mut self, ws: &str, f: impl FnOnce(&mut Vec<Comment>)) {
        self.load_comments(ws);
        let l = self.comments.get_mut(ws).unwrap();
        let before = l.clone();
        f(l);
        if *l != before || self.failed_saves.contains_key(ws) {
            self.save_comments(ws);
        }
    }
}

/// A small status chip: a colored dot and a label.
fn chip(label: String, color: Hsla, theme: &Theme) -> Div {
    h_flex()
        .h(px(22.))
        .px_2()
        .gap_1p5()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(div().size(px(6.)).rounded_full().bg(color))
        .child(label)
}

/// A composer box holding `text`, focused with the cursor at its end.
fn textarea(
    window: &mut Window,
    text: String,
    placeholder: &'static str,
    cx: &mut Context<Differ>,
) -> Entity<TextareaState> {
    let t = cx.new(|cx| {
        TextareaState::new(window, cx)
            .auto_grow(1, 12)
            .placeholder(placeholder)
            .default_value(text)
    });
    t.update(cx, |i, cx| {
        let end = i.value().len();
        i.set_selected_range(end..end, cx);
        i.focus(window, cx)
    });
    t
}

impl Differ {
    fn diff_rows(&self, cx: &App) -> Vec<Row> {
        let d = self.diff.read(cx);
        let files: Vec<(SharedString, &[Hunk], bool)> = (self.diff_files.iter())
            .map(|f| {
                let hunks = self.review.hunks.get(f.path()).map_or(&[][..], |h| &h[..]);
                (f.path().clone(), hunks, d.is_file_collapsed(f.path()))
            })
            .collect();
        rows(&files, d.mode() == DiffMode::Split)
    }

    /// Scrolls `top` to the top of the diff and shows the cursor as the selected line.
    fn apply_view(&mut self, rows: &[Row], top: usize, cur: usize, cx: &mut Context<Self>) {
        let (Some(t), Some(c)) = (rows.get(top), rows.get(cur)) else {
            return;
        };
        let (t, c) = (t.spot.clone(), c.spot.clone());
        if let Some(ix) = self.diff_files.iter().position(|f| f.path() == c.path()) {
            self.diff_file = ix; // `v`/`o` act on the cursor's file
            self.follow_tree(ix);
        }
        self.diff.update(cx, |d, cx| {
            match &t {
                Spot::File(p) => d.scroll_to_file(p, cx),
                Spot::Line(l) => d.scroll_to_line(l.clone(), cx),
            }
            let sel = match &c {
                Spot::Line(l) => Some(DiffLineRange::new(
                    l.path().clone(),
                    l.side(),
                    l.line(),
                    l.line(),
                )),
                Spot::File(_) => None,
            };
            d.set_selected_lines(sel, cx);
        });
        self.review.view = Some((t, c));
        cx.notify();
    }

    /// A code row's height.
    fn row_height(&self, cx: &App) -> Pixels {
        crate::editor::diff_font(cx).1
    }

    fn view_cap(&self, cx: &App) -> f32 {
        let h = self.review.viewport.get().size.height;
        (h / self.row_height(cx)).max(SCROLLOFF * 2. + 1.)
    }

    /// `j/k` (`n` = ±1), `ctrl-d/u` (± half a screen), `gg`/`G` (`to` = first/last row).
    pub(crate) fn diff_move(
        &mut self,
        n: isize,
        to: Option<bool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cap = self.view_cap(cx);
        let n = match n.abs() {
            1 => n,
            _ => n.signum() * (cap / 2.) as isize,
        };
        let Some((top, cur)) = self.review.view.clone().or_else(|| {
            let first = Spot::File(self.diff_files.first()?.path().clone());
            to.map(|_| (first.clone(), first))
        }) else {
            // The mouse moved the diff: we don't know where it is, so scroll natively.
            let name = match (n > 0, n.abs() == 1) {
                (true, true) => "shark_diff::ScrollDown",
                (false, true) => "shark_diff::ScrollUp",
                (true, false) => "shark_diff::PageDown",
                (false, false) => "shark_diff::PageUp",
            };
            if let Ok(a) = cx.build_action(name, None) {
                window.dispatch_action(a, cx);
            }
            return;
        };
        let rows = self.diff_rows(cx);
        if rows.is_empty() {
            return;
        }
        let (t, c) = (find(&rows, &top), find(&rows, &cur));
        let c = match to {
            Some(true) => 0,
            Some(false) => rows.len() - 1,
            None => c.saturating_add_signed(n).min(rows.len() - 1),
        };
        let t = fit(&rows, t, c, cap);
        self.drop_mouse_selection(window, cx);
        self.apply_view(&rows, t, c, cx);
    }

    /// `]c` / `[c`: the cursor goes to the next change; natively without a keyboard view.
    pub(crate) fn diff_change(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((top, cur)) = self.review.view.clone() else {
            return self.diff.update(cx, |d, cx| match forward {
                true => d.next_change(cx),
                false => d.previous_change(cx),
            });
        };
        let rows = self.diff_rows(cx);
        let Some(c) = next_change(&rows, find(&rows, &cur), forward) else {
            return;
        };
        let t = fit(&rows, find(&rows, &top), c, self.view_cap(cx));
        self.drop_mouse_selection(window, cx);
        self.apply_view(&rows, t, c, cx);
    }

    /// After `set_files` reset the diff, put the keyboard view back; false when there is none.
    pub(crate) fn restore_view(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((top, cur)) = self.review.view.clone() else {
            return false;
        };
        let rows = self.diff_rows(cx);
        let (t, c) = (find(&rows, &top), find(&rows, &cur));
        self.apply_view(&rows, t, c, cx);
        true
    }

    /// The mouse scrolled: the view model no longer matches the screen.
    pub(crate) fn lose_view(&mut self) {
        self.review.view = None;
    }

    /// Tracks the pane's height for paging; goes in the diff pane.
    pub(crate) fn viewport_probe(&self) -> impl IntoElement {
        let (vp, width) = (self.review.viewport.clone(), self.media_width.clone());
        canvas(
            move |b, _, _| {
                vp.set(b);
                width.set(b.size.width);
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full()
    }

    pub(crate) fn ws_comments(&self) -> &[Comment] {
        self.workspace_name()
            .and_then(|ws| self.review.comments.get(ws))
            .map_or(&[], |c| &c[..])
    }

    /// The open workspace's comment `id`.
    pub(crate) fn comment(&self, id: u64) -> Option<&Comment> {
        self.ws_comments().iter().find(|c| c.id == id)
    }

    /// Edits the open workspace's comments and saves them.
    pub(crate) fn edit_comments(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Vec<Comment>),
    ) {
        if let Some(ws) = self.workspace_name().map(str::to_string) {
            self.review.edit(&ws, f);
            cx.notify();
        }
    }

    /// Threads (not resolved) and the composer as diff annotations under their last line, and
    /// the before/after row of each image or video under its header (not while folded). File
    /// headers get `review_chips` through `card_header_suffix`.
    pub(crate) fn with_comments(&self, diff: Diff, cx: &mut Context<Self>) -> Diff {
        let editing = self.review.composer.as_ref().map(|c| c.draft.id);
        let state = self.diff.read(cx);
        // Indexed as in `self.media`, which `render_annotation` reads.
        let media: Vec<_> = (self.media.iter().enumerate())
            .filter(|(_, (path, _))| !state.is_file_collapsed(path))
            .map(|(i, (path, _))| (i, path.clone()))
            .collect();
        // On their line while the diff has it; else under their file's header (the rest show
        // below the diff, `render_orphans`).
        let threads = (self.ws_comments().iter())
            .filter(|c| c.shown() && Some(c.id) != editing)
            .filter_map(|c| {
                let id = ("thread", c.id as usize);
                match self.on_its_line(c) {
                    true => Some(DiffAnnotation::line(id, c.anchor())),
                    false => (self.diff_files.iter().any(|f| *f.path() == c.path))
                        .then(|| DiffAnnotation::file(id, c.path.clone())),
                }
            });
        let composer = (self.review.composer.as_ref())
            .map(|c| DiffAnnotation::line(("composer", 0usize), c.draft.anchor()));
        let shark = cx.entity().downgrade();
        let add = shark.clone();
        // The diff's + beside a line or a selection: the composer there.
        diff.on_add_annotation(move |range, window, cx| {
            add.update(cx, |s, cx| {
                s.diff
                    .update(cx, |d, cx| d.set_selected_lines(Some(range.clone()), cx));
                s.comment_at(range.clone(), window, cx)
            })
            .ok();
        })
        .annotation_hints("Comment on working copy →", "← Comment on original")
        .annotations(threads.chain(composer).chain(
            (media.iter()).map(|(i, path)| DiffAnnotation::file(("media", *i), path.clone())),
        ))
        .render_annotation(move |a, _, cx| {
            let (name, i) = match a.id() {
                ElementId::NamedInteger(name, i) => (name.to_string(), *i as usize),
                _ => (String::new(), 0),
            };
            shark
                .update(cx, |s, cx| match name.as_str() {
                    "media" => (s.media.get(i).map(|(_, v)| v.clone().into_any_element()))
                        .unwrap_or_else(|| div().into_any_element()),
                    "composer" => s.render_composer(cx),
                    _ => s.render_thread(i as u64, cx),
                })
                .unwrap_or_else(|_| div().into_any_element())
        })
    }

    /// Whether the diff shows the thread's line (in a hunk, on its side).
    pub(crate) fn on_its_line(&self, c: &Comment) -> bool {
        self.hunk_of(c, c.end.1).is_some()
    }

    /// Whether one hunk of the diff shows all the thread's lines, as GitHub wants an inline
    /// comment's.
    pub(crate) fn in_one_hunk(&self, c: &Comment) -> bool {
        let h = self.hunk_of(c, c.end.1);
        h.is_some() && self.hunk_of(c, c.start.1) == h
    }

    /// The hunk of the diff showing line `line` of the comment's side. Old-side numbers count
    /// in the commit the comment was made against, so another compare commit shows none.
    fn hunk_of(&self, c: &Comment, line: usize) -> Option<usize> {
        let now = (c.path.split_once('/')).and_then(|(repo, _)| self.diff_revs.get(repo));
        if c.end.0 && now.is_some_and(|r| !r.is_empty() && !c.rev.is_empty() && *r != c.rev) {
            return None;
        }
        (self.review.hunks.get(c.path.as_str()))?
            .iter()
            .position(|h| {
                h.iter().any(|&(k, old, new)| match c.end.0 {
                    true => k != b'+' && old == line,
                    false => k != b'-' && new == line,
                })
            })
    }

    /// Moves workspace `ws`'s working-copy threads to where their lines are on disk now, so
    /// lines added above one don't leave it beside other code.
    pub(crate) fn reanchor_comments(&mut self, ws: &str) {
        let Some(dir) = self
            .workspace
            .as_ref()
            .filter(|w| w.name == ws)
            .map(Workspace::dir)
        else {
            return;
        };
        // ponytail: reads each thread's file on every diff refresh; group by path if many.
        self.review.edit(ws, |l| {
            for c in l.iter_mut().filter(|c| c.shown() && !c.end.0) {
                lines_now(&dir, c);
            }
        });
    }

    /// Threads whose file left the diff, below it, so none goes missing. Pending ones get
    /// Edit and Delete there, as no line cursor reaches them; edited, the composer shows in
    /// the card's place.
    pub(crate) fn render_orphans(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        // No diff (yet, or git failed): every thread would look orphaned.
        if !self.diff_loaded || self.diff_error.is_some() {
            return None;
        }
        let ids: Vec<u64> = (self.ws_comments().iter())
            .filter(|c| c.shown() && !self.diff_files.iter().any(|f| *f.path() == c.path))
            .map(|c| c.id)
            .collect();
        if ids.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let title = div()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child("Threads on files no longer in the diff");
        let cards: Vec<AnyElement> = (ids.iter())
            .map(|&id| {
                let c = self.comment(id).map(|c| c.path.clone()).unwrap_or_default();
                let pending = self
                    .comment(id)
                    .is_some_and(|c| c.status == Status::Pending);
                let editing = (self.review.composer.as_ref()).is_some_and(|c| c.draft.id == id);
                let button = |name: &'static str, label: &'static str| {
                    Button::new((name, id as usize))
                        .ghost()
                        .xsmall()
                        .label(label)
                };
                v_flex()
                    .p_3()
                    .gap_1()
                    .child(div().text_xs().child(c))
                    .child(match editing {
                        true => self.render_composer(cx),
                        false => self.render_thread(id, cx),
                    })
                    .when(pending && !editing, |d| {
                        d.child(
                            h_flex()
                                .gap_2()
                                .justify_end()
                                .child(button("orphan-edit", "Edit").on_click(
                                    cx.listener(move |this, _, w, cx| this.edit_comment(id, w, cx)),
                                ))
                                .child(button("orphan-delete", "Delete").on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.edit_comments(cx, |l| l.retain(|c| c.id != id))
                                    },
                                ))),
                        )
                    })
                    .into_any_element()
            })
            .collect();
        Some(
            v_flex()
                .id("orphan-threads")
                .flex_none()
                .max_h(px(240.))
                .overflow_y_scroll()
                .px_4()
                .pt_2()
                .border_t_1()
                .border_color(cx.theme().border)
                .child(title)
                .children(cards)
                .into_any_element(),
        )
    }

    /// The comment quoting its lines as they are now (`snapshot`), at the revision they are
    /// read at (`rev`).
    fn anchor_text(&self, c: &Comment) -> Comment {
        let Some(ws) = self.active_ws() else {
            return c.clone();
        };
        let rev = match c.end.0 {
            false => "working tree".to_string(),
            true => (c.path.split_once('/'))
                .map(|(repo, _)| {
                    // ponytail: merge-base (and lines_now's git show) block the UI thread as
                    // the composer opens; move them to the background executor if big repos lag.
                    compare_rev(
                        &ws.dir().join(repo),
                        self.diff_base(repo),
                        self.preferences.uncommitted_only,
                    )
                })
                .unwrap_or_default(),
        };
        // A file being edited in place shows its buffer, not what is on disk.
        let edited = (self.editing.as_ref())
            .filter(|e| !c.end.0 && c.start.0 == c.end.0 && *e.path == c.path)
            .map(|e| slice(&e.buf.text(), c.start.1, c.end.1));
        let mut c = Comment { rev, ..c.clone() };
        let now = edited.or_else(|| lines_now(&ws.dir(), &mut c));
        Comment {
            snapshot: now.unwrap_or_default(),
            ..c
        }
    }

    /// `c`: the composer on the selected lines (the cursor line). A pending thread there is
    /// edited instead.
    pub(crate) fn comment_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.is_none() {
            return notify_err(window, cx, "Open a workspace first");
        }
        let Some(range) = self.picked_lines(cx) else {
            let keys = if self.preferences.vim {
                "j/k"
            } else {
                "↑/↓"
            };
            return notify_err(
                window,
                cx,
                format!("Select code or lines first, or move to a line ({keys})"),
            );
        };
        self.comment_at(range, window, cx)
    }

    /// `c` is on working-copy lines with unsaved edits in place: not what the agent reads.
    pub(crate) fn unsaved_edit(&self, c: &Comment) -> bool {
        (!c.start.0 || !c.end.0)
            && (self.editing.as_ref()).is_some_and(|e| e.dirty && *e.path == c.path)
    }

    /// Code selected with the mouse, else the line cursor or selected lines.
    fn picked_lines(&self, cx: &App) -> Option<DiffLineRange> {
        let d = self.diff.read(cx);
        d.text_selection_lines(cx).or_else(|| d.selected_lines())
    }

    /// A key moved the line cursor: a mouse selection left behind is no longer what `c` means.
    fn drop_mouse_selection(&self, window: &mut Window, cx: &mut App) {
        if self.diff.read(cx).text_selection_lines(cx).is_some() {
            gpui_kit::base::TextSelection::clear(window, cx);
        }
    }

    /// The composer on `range`, whatever is selected (the header's Comment, the diff's +).
    fn comment_at(&mut self, range: DiffLineRange, window: &mut Window, cx: &mut Context<Self>) {
        let draft = Comment::from_range(&range);
        if self.unsaved_edit(&draft) {
            return notify_err(
                window,
                cx,
                "Save first (⌘S): the agent reads the file on disk",
            );
        }
        self.keep_draft(window, cx);
        let same = (self.ws_comments().iter())
            .find(|c| {
                c.status == Status::Pending
                    && (&c.path, c.start, c.end) == (&draft.path, draft.start, draft.end)
            })
            .cloned();
        let c = same.unwrap_or_else(|| self.anchor_text(&draft));
        self.open_composer(c, window, cx)
    }

    /// A composer open elsewhere with text in it is added (or saved), not lost, before
    /// another opens.
    pub(crate) fn keep_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = |c: &Composer| !c.input.read(cx).value().trim().is_empty();
        if self.review.composer.as_ref().is_some_and(typed) {
            self.submit_composer(window, cx);
        }
    }

    /// `e`, an orphan card's Edit: the composer on thread `id`, keeping an open draft.
    fn edit_comment(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.keep_draft(window, cx);
        if let Some(c) = self.comment(id).cloned() {
            self.open_composer(c, window, cx);
        }
    }

    /// Opens the composer on `c`: an existing comment (id set) or a new draft, scrolled into
    /// view above the review bar.
    fn open_composer(&mut self, c: Comment, window: &mut Window, cx: &mut Context<Self>) {
        // The text box last: it takes the focus.
        let suggest =
            (c.suggestion.clone()).map(|s| textarea(window, s, "The code you propose", cx));
        let input = textarea(
            window,
            c.text.clone(),
            "What should the agent change here?",
            cx,
        );
        let anchor = Spot::Line(c.anchor());
        self.review.composer = Some(Composer {
            draft: c,
            input,
            suggest,
            height: Rc::default(),
        });
        // Room for most of it, so it is laid out; then `render_composer` measures it. An
        // orphan's shows below the diff.
        if self.diff_files.iter().any(|f| f.path() == anchor.path()) {
            self.reveal(&anchor, 8., cx);
        }
        cx.notify();
    }

    /// The diff scrolls `by` down (in the diff's own steps, so a little more), the composer
    /// with it. Threads and headers aren't the row model's height, so this goes by pixels.
    /// The diff scrolls only while it has the focus: it borrows it from the composer.
    fn scroll_diff(&mut self, by: Pixels, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(back), Ok(down)) = (
            window.focused(cx),
            cx.build_action("shark_diff::ScrollDown", None),
        ) else {
            return;
        };
        let diff = self.diff.read(cx).focus_handle(cx);
        diff.focus(window, cx);
        for _ in 0..(by / (window.rem_size() * 1.5)).ceil() as usize {
            diff.dispatch_action(&*down, window, cx);
        }
        back.focus(window, cx);
    }

    /// cmd-shift-s: the Suggest change box, prefilled with the lines; again closes it.
    pub(crate) fn toggle_suggest(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(comp) = &mut self.review.composer else {
            return;
        };
        match comp.suggest.take() {
            Some(_) => comp.input.update(cx, |i, cx| i.focus(window, cx)),
            None => {
                let text = comp.draft.snapshot.clone();
                comp.suggest = Some(textarea(window, text, "The code you propose", cx));
            }
        }
        cx.notify();
    }

    /// cmd-enter: adds the draft to the review (Pending), or saves the edited thread. Empty
    /// text removes it.
    pub(crate) fn submit_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(comp) = self.review.composer.take() else {
            return;
        };
        let text = comp.input.read(cx).value().trim().to_string();
        let suggestion = (comp.suggest.as_ref())
            .map(|s| s.read(cx).value().trim_end().to_string())
            .filter(|s| !s.trim().is_empty());
        let order: HashMap<String, usize> = (self.diff_files.iter().enumerate())
            .map(|(i, f)| (f.path().to_string(), i))
            .collect();
        let Some(ws) = self.workspace_name().map(str::to_string) else {
            return;
        };
        let id = match comp.draft.id {
            0 => self.review.new_id(&ws),
            id => id,
        };
        self.edit_comments(cx, |list| {
            let old = list.iter().position(|c| c.id == id).map(|i| list.remove(i));
            if !text.is_empty() {
                let mut old = old.unwrap_or(comp.draft);
                // New words the agent hasn't seen: pending again, to send.
                if old.text != text || old.suggestion != suggestion {
                    (old.status, old.chat) = (Status::Pending, None);
                }
                list.push(Comment {
                    id,
                    text,
                    suggestion,
                    ..old
                });
                // File order, then line, so the prompt reads top to bottom.
                list.sort_by_key(|c| (order.get(&c.path).copied(), c.start.1));
            }
        });
        self.focus_diff(window, cx);
    }

    /// Esc: drops the draft (an edited thread stays as it was).
    pub(crate) fn cancel_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.review.composer = None;
        self.focus_diff(window, cx);
        cx.notify();
    }

    pub(crate) fn focus_diff(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.diff.read(cx).focus_handle(cx).focus(window, cx);
    }

    /// The shown thread under the line cursor (or the selection's last line).
    pub(crate) fn thread_at(&self, cx: &App) -> Option<u64> {
        let r = self.diff.read(cx).selected_lines()?;
        let at = DiffLinePosition::new(r.path().clone(), r.end_side(), r.end());
        let here: Vec<u64> = (self.ws_comments().iter())
            .filter(|c| c.shown() && c.covers(&at))
            .map(|c| c.id)
            .collect();
        (self.review.jumped.as_ref())
            .filter(|(id, sel)| *sel == r && here.contains(id))
            .map(|(id, _)| *id)
            .or(here.first().copied())
    }

    /// Thread `id`, unless it waits in a chat's queued review card (that card would send it
    /// as it was, or not at all).
    fn unqueued(&self, id: u64, window: &mut Window, cx: &mut Context<Self>) -> Option<Comment> {
        let c = self.comment(id)?;
        if c.status == Status::Queued {
            notify_err(
                window,
                cx,
                "Queued in a chat · remove the review from its queue first",
            );
            return None;
        }
        Some(c.clone())
    }

    /// Runs a thread action (`e`, `x`, `d`) on the thread at the cursor.
    pub(crate) fn at_cursor(
        &mut self,
        f: fn(&mut Self, u64, &mut Window, &mut Context<Self>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.thread_at(cx) {
            f(self, id, window, cx);
        }
    }

    pub(crate) fn edit_thread(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.unqueued(id, window, cx).is_some() {
            self.edit_comment(id, window, cx);
        }
    }

    pub(crate) fn delete_thread(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.unqueued(id, window, cx).is_some() {
            self.edit_comments(cx, |l| l.retain(|c| c.id != id));
        }
    }

    pub(crate) fn set_status(&mut self, ids: &[u64], status: Status, cx: &mut Context<Self>) {
        self.edit_comments(cx, |l| {
            for c in l.iter_mut().filter(|c| ids.contains(&c.id)) {
                c.status = status.clone();
            }
        });
    }

    /// `r`: resolve the sent thread at the cursor (a pending one has no way back from hidden);
    /// with none, `r` refreshes the diff as before.
    pub(crate) fn resolve_thread(&mut self, cx: &mut Context<Self>) {
        match (self.thread_at(cx).and_then(|id| self.comment(id))).filter(|c| c.status.resolvable())
        {
            Some(c) => self.set_status(&[c.id], Status::Resolved, cx),
            None => cx.propagate(),
        }
    }

    /// `R`: back to pending, to send again.
    pub(crate) fn reopen_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = (self.thread_at(cx)).filter(|&id| self.unqueued(id, window, cx).is_some())
        {
            self.set_status(&[id], Status::Pending, cx);
        }
    }

    fn with_status(&self, f: impl Fn(&Status) -> bool) -> Vec<u64> {
        (self.ws_comments().iter())
            .filter(|c| f(&c.status))
            .map(|c| c.id)
            .collect()
    }

    /// Outdated threads pending again, quoting their lines as they are now.
    pub(crate) fn requeue_outdated(&mut self, cx: &mut Context<Self>) {
        let mut now: HashMap<u64, Comment> = (self.ws_comments().iter())
            .filter(|c| c.status == Status::Outdated)
            .map(|c| (c.id, self.anchor_text(c)))
            .collect();
        self.edit_comments(cx, |l| {
            for c in l.iter_mut() {
                if let Some(n) = now.remove(&c.id) {
                    *c = Comment {
                        status: Status::Pending,
                        ..n
                    };
                }
            }
        });
    }

    pub(crate) fn resolve_addressed(&mut self, cx: &mut Context<Self>) {
        let ids = self.with_status(|s| matches!(s, Status::Addressed { .. }));
        self.set_status(&ids, Status::Resolved, cx);
    }

    /// The review bar's Discard: every pending comment, once confirmed.
    pub(crate) fn discard_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.with_status(|s| *s == Status::Pending).len();
        confirm(
            format!(
                "Discard {n} pending comment{}?",
                if n == 1 { "" } else { "s" }
            ),
            "They have not been sent to a chat or GitHub.",
            "Discard",
            |s, _, cx| s.edit_comments(cx, |l| l.retain(|c| c.status != Status::Pending)),
            window,
            cx,
        );
    }

    /// The diff file thread `id` is on; none once its file left the diff.
    fn change_ix(&self, id: u64) -> Option<usize> {
        let path = &self.comment(id)?.path;
        self.diff_files.iter().position(|f| f.path() == path)
    }

    /// The thread's file in the editor, to see what the agent made of it.
    pub(crate) fn view_change(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.change_ix(id) {
            self.open_file(ix, window, cx);
        }
    }

    /// A thread card's right-click menu. Shortcuts stay in the card's key hints.
    pub(crate) fn thread_menu(&self, id: u64, cx: &Context<Self>) -> Vec<Entry> {
        let Some(c) = self.comment(id).cloned() else {
            return vec![];
        };
        let act = |f: fn(&mut Self, u64, &mut Window, &mut Context<Self>)| {
            on_differ(self.workspace_name(), cx, move |s, w, cx| f(s, id, w, cx))
        };
        let addressed = matches!(c.status, Status::Addressed { .. } | Status::Outdated);
        let mut entries = vec![menu::item(IconName::Pencil, "Edit", act(Self::edit_thread))];
        if c.status.resolvable() {
            entries.push(menu::item(
                IconName::CircleCheck,
                "Resolve",
                act(|s, id, _, cx| s.set_status(&[id], Status::Resolved, cx)),
            ));
        }
        if addressed || c.status == Status::Resolved {
            entries.push(menu::item(
                IconName::CircleDashed,
                "Reopen",
                act(|s, id, _, cx| s.set_status(&[id], Status::Pending, cx)),
            ));
        }
        if addressed && self.change_ix(id).is_some() {
            let view = act(Self::view_change);
            entries.push(menu::item(IconName::FileDiff, "View change", view));
        }
        if matches!(c.status, Status::Queued | Status::Sent { .. }) || addressed {
            let show = act(Self::show_thread_chat);
            entries.push(menu::item(IconName::MessageSquare, "Show chat", show));
        }
        if c.status != Status::Pending {
            entries.push(match c.github {
                Some(url) => menu::item(IconName::Github, "View on GitHub", move |_, cx| {
                    cx.open_url(&url)
                }),
                None => menu::item(IconName::Github, "Share to GitHub…", act(Self::open_share)),
            });
        }
        let text = c.text;
        entries.extend([
            menu::item(IconName::Copy, "Copy comment", move |w, cx| {
                copy(text.clone(), w, cx)
            }),
            Entry::Sep,
            menu::item(IconName::Trash, "Delete", act(Self::delete_thread)),
        ]);
        entries
    }

    /// Scrolls row `spot` into view with `below` code rows of room under it (for what is
    /// annotated there), keeping the selected lines.
    fn reveal(&mut self, spot: &Spot, below: f32, cx: &mut Context<Self>) {
        let sel = self.diff.read(cx).selected_lines();
        let rows = self.diff_rows(cx);
        let cur = find(&rows, spot);
        let top = (self.review.view.as_ref()).map_or(cur, |(t, _)| find(&rows, t));
        let cap = (self.view_cap(cx) - below).max(SCROLLOFF * 2. + 1.);
        self.apply_view(&rows, fit(&rows, top, cur, cap), cur, cx);
        self.diff.update(cx, |d, cx| d.set_selected_lines(sel, cx));
    }

    /// Puts the line cursor on comment `id`.
    pub(crate) fn jump_to_comment(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.comment(id).cloned() else {
            return;
        };
        // Orphans have no row to go to; they show below the diff.
        if !self.diff_files.iter().any(|f| *f.path() == c.path) {
            return notify_err(
                window,
                cx,
                "Its file is no longer in the diff: see the threads below it",
            );
        }
        self.drop_mouse_selection(window, cx);
        // A viewed file is folded: its line has no row.
        self.diff.update(cx, |d, cx| {
            if d.is_file_collapsed(&c.path) {
                d.set_file_collapsed(&c.path, false, cx)
            }
        });
        self.reveal(&Spot::Line(c.anchor()), 0., cx);
        // `find` lands on the closest row; the thread's own line is what `e`/`x` read.
        let sel = DiffLineRange::new(c.path.clone(), side(c.end.0), c.end.1, c.end.1);
        self.diff
            .update(cx, |d, cx| d.set_selected_lines(Some(sel), cx));
        self.review.jumped = self.diff.read(cx).selected_lines().map(|s| (id, s));
    }

    /// `]c` / `[c`: the next (previous) thread, wrapping; with none, the next change.
    pub(crate) fn thread_jump(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Top to bottom as the diff shows them; one row's by id. Orphans (their file left the
        // diff) have no row, so they are skipped.
        let rows = self.diff_rows(cx);
        let row = |l: DiffLinePosition| find(&rows, &Spot::Line(l));
        let mut threads: Vec<((usize, u64), u64)> = (self.ws_comments().iter())
            .filter(|c| c.shown() && self.diff_files.iter().any(|f| *f.path() == c.path))
            .map(|c| ((row(c.anchor()), c.id), c.id))
            .collect();
        if threads.is_empty() {
            return self.diff_change(forward, window, cx);
        }
        threads.sort();
        let here = self.thread_at(cx).unwrap_or(0);
        let at = (self.diff.read(cx).selected_lines()).map(|r| {
            let l = DiffLinePosition::new(r.path().clone(), r.end_side(), r.end());
            (row(l), here)
        });
        let next = match (forward, at) {
            (_, None) => threads.first(),
            (true, Some(at)) => threads.iter().find(|t| t.0 > at).or(threads.first()),
            (false, Some(at)) => threads.iter().rev().find(|t| t.0 < at).or(threads.last()),
        };
        if let Some(&(_, id)) = next {
            self.jump_to_comment(id, window, cx);
        }
    }

    /// "Show chat ↗": the chat the thread went to (else the open one).
    pub(crate) fn show_thread_chat(
        &mut self,
        id: u64,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chat = self.comment(id).and_then(|c| c.chat.clone());
        if let Some(workspace) = self.workspace_name() {
            cx.emit(DifferEvent::OpenChat {
                workspace: workspace.into(),
                chat,
            });
        }
    }

    /// A chat turn ended: its reply's reports and the code decide what became of the comments
    /// it was sent. Read from disk, so it doesn't wait for the diff refresh.
    pub(crate) fn review_turn_done(
        &mut self,
        ws: &str,
        dir: &Path,
        id: &str,
        turn: usize,
        reply: &str,
        held: &[u64],
        cx: &mut Context<Self>,
    ) {
        let reports: HashMap<u64, String> = parse_reports(reply).into_iter().collect();
        self.review.edit(ws, |list| {
            for c in list.iter_mut() {
                let ours = c.chat.as_deref() == Some(id) && !held.contains(&c.id);
                if !ours || !matches!(c.status, Status::Sent { .. }) {
                    continue;
                }
                let now = (!c.end.0).then(|| lines_now(&dir, c)).flatten();
                let report = reports.get(&c.id).map(|s| s.as_str());
                c.status = judge(&c.status, turn, report, now.as_deref(), &c.snapshot);
            }
        });
        cx.notify();
    }

    /// Chat `chat` sent comments `ids` (`Some(turn)`: read in that turn), or they never
    /// went out (`None`: removed from its queue, or never read before its process ended) and
    /// are pending again. Only comments still waiting in that chat change: an id may be gone.
    pub(crate) fn review_delivered(
        &mut self,
        ws: &str,
        chat: &str,
        ids: &[u64],
        turn: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        self.review.edit(ws, |l| {
            let ours = |c: &Comment| ids.contains(&c.id) && c.chat.as_deref() == Some(chat);
            for c in l.iter_mut().filter(|c| ours(c)) {
                match (turn, &c.status) {
                    (Some(turn), Status::Queued) => c.status = Status::Sent { turn },
                    (None, Status::Queued | Status::Sent { .. }) => {
                        (c.status, c.chat) = (Status::Pending, None)
                    }
                    _ => {}
                }
            }
        });
        cx.notify();
    }

    /// The composer (A·01): lines and side, Suggest change, the text, Esc / Add to review.
    fn render_composer(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(comp) = &self.review.composer else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let editing = comp.draft.id != 0;
        let (seen, vp) = (comp.height.clone(), self.review.viewport.clone());
        let shark = cx.entity().downgrade();
        // Measured after layout: taller than last time (opened, grew) and past the pane's
        // bottom, the diff scrolls up to show it, but never its top out of view.
        let probe = canvas(
            move |b, window, cx| {
                if b.size.height <= seen.get() {
                    return;
                }
                seen.set(b.size.height);
                let pane = vp.get();
                let by = (b.bottom() - pane.bottom()).min(b.top() - pane.top());
                if by > px(0.) && b.top() < pane.bottom() {
                    window.defer(cx, move |w, cx| {
                        (shark.update(cx, |s, cx| s.scroll_diff(by, w, cx))).ok();
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        v_flex()
            .id("composer-card")
            .key_context("ReviewComposer")
            .min_w_0()
            .overflow_hidden()
            .mx(px(-12.))
            .my(px(-12.))
            .px_4()
            .py_3()
            .gap_2()
            .border_l_2()
            .border_color(theme.primary)
            .text_sm()
            // cmd-enter would add a line and Esc stops in the textarea; both are ours here.
            .capture_action(cx.listener(|this, a: &Enter, w, cx| {
                if a.secondary {
                    cx.stop_propagation();
                    this.submit_composer(w, cx);
                }
            }))
            .capture_action(cx.listener(|this, _: &Escape, w, cx| {
                cx.stop_propagation();
                this.cancel_composer(w, cx);
            }))
            .on_action(cx.listener(|this, _: &SuggestChange, w, cx| this.toggle_suggest(w, cx)))
            .child(probe)
            // Narrow, the buttons wrap below rather than squeeze.
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(comp.draft.lines()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .child(comp.draft.side()),
                    )
                    .child(
                        Button::new("suggest-change")
                            .outline()
                            .xsmall()
                            .ml_auto()
                            .flex_shrink_1()
                            .min_w_0()
                            .label("Suggest change ⌘⇧S")
                            .on_click(cx.listener(|this, _, w, cx| this.toggle_suggest(w, cx))),
                    ),
            )
            .child(
                div()
                    .px_2()
                    .py_1()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.primary)
                    .child(Textarea::new(&comp.input).appearance(false)),
            )
            .when_some(comp.suggest.as_ref(), |d, s| {
                d.child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.muted)
                        .font_family(theme.mono_font_family.clone())
                        .child(Textarea::new(s).appearance(false)),
                )
            })
            .child(
                h_flex()
                    .flex_wrap()
                    .justify_end()
                    .gap_2()
                    .child(
                        div()
                            .flex_grow(1.)
                            .min_w_0()
                            .whitespace_normal()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(match editing {
                                true => "Empty text deletes the thread",
                                false => "Stays pending until you send the review",
                            }),
                    )
                    .child(
                        Button::new("composer-esc")
                            .ghost()
                            .xsmall()
                            .flex_shrink_1()
                            .min_w_0()
                            .label("Esc")
                            .on_click(cx.listener(|this, _, w, cx| this.cancel_composer(w, cx))),
                    )
                    .child(
                        Button::new("composer-add")
                            .primary()
                            .xsmall()
                            .flex_shrink_1()
                            .min_w_0()
                            .label(if editing {
                                "Save ⌘↵"
                            } else {
                                "Add to review ⌘↵"
                            })
                            .on_click(cx.listener(|this, _, w, cx| this.submit_composer(w, cx))),
                    ),
            )
            .test_support()
            .into_any_element()
    }

    /// A thread card (A·02, A·04, 05, 08).
    fn render_thread(&self, id: u64, cx: &mut Context<Self>) -> AnyElement {
        let Some(c) = self.comment(id).cloned() else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let (pill, color, hints) = match &c.status {
            Status::Pending => (
                "Pending".to_string(),
                theme.warning_foreground,
                "e edit · x delete",
            ),
            Status::Queued => ("Queued".into(), theme.primary, ""),
            Status::Sent { .. } => ("Sent".into(), theme.primary, "r resolve · e edit"),
            Status::Addressed { turn, .. } => (
                format!("Addressed · turn {turn}"),
                theme.success_foreground,
                "d view change · r resolve · R reopen",
            ),
            Status::Outdated => (
                "Outdated".into(),
                theme.muted_foreground,
                "d view change · r resolve · R reopen",
            ),
            Status::Resolved => ("Resolved".into(), theme.muted_foreground, ""),
        };
        let in_context = matches!(
            c.status,
            Status::Queued | Status::Sent { .. } | Status::Addressed { .. } | Status::Outdated
        );
        // Off its line (gone from the diff): the lines as they were, so it still reads.
        let snapshot =
            (!self.on_its_line(&c) && !c.snapshot.is_empty()).then(|| c.snapshot.clone());
        let posted = c.github.as_deref().map(posted_as);
        let where_ = match (in_context, posted.is_some()) {
            (true, true) => "In AI context · On GitHub",
            (true, false) => "In AI context",
            (false, true) => "On GitHub",
            (false, false) => "",
        };
        let mono = theme.mono_font_family.clone();
        v_flex()
            .id(("thread-card", id as usize))
            .min_w_0()
            .overflow_hidden()
            .mx(px(-12.))
            .my(px(-12.))
            .px_4()
            .py_3()
            .gap_1p5()
            .border_l_2()
            .border_color(color)
            .text_sm()
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &MouseDownEvent, w, cx| {
                    cx.stop_propagation();
                    // Selects it like `]c`; a thread whose file left the diff has no line to select.
                    if this.change_ix(id).is_some() {
                        this.jump_to_comment(id, w, cx);
                    }
                    let entries = this.thread_menu(id, cx);
                    this.show_menu(e.position, entries, w, cx);
                }),
            )
            // Narrow, the pill and where it went wrap to the next line rather than clip; the
            // keys take what room is left on the last line, the lines truncate last.
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_x_2()
                    .child(
                        div()
                            .flex_none()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("You"),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .child(format!("{} · {}", c.lines(), c.side())),
                    )
                    .child(
                        div()
                            .id(("thread-pill", id as usize))
                            .min_w_0()
                            .truncate()
                            .px_1p5()
                            .rounded(theme.radius)
                            .text_xs()
                            .text_color(color)
                            .bg(color.opacity(0.12))
                            .child(pill)
                            .test_support(),
                    )
                    .child(
                        div()
                            .id(("thread-where", id as usize))
                            .min_w_0()
                            .whitespace_normal()
                            .text_xs()
                            .text_color(theme.success_foreground)
                            .child(where_)
                            .test_support(),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_right()
                            .text_xs()
                            .text_color(faint(theme))
                            .child(hints),
                    ),
            )
            .when_some(snapshot, |d, s| {
                d.child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .font_family(mono.clone())
                        .text_size(theme.mono_font_size)
                        .text_color(theme.muted_foreground)
                        .child(s),
                )
            })
            .child(
                div()
                    .text_color(theme.foreground)
                    .whitespace_normal()
                    .child(c.text.clone()),
            )
            .when_some(c.suggestion.clone(), |d, s| {
                d.child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("Suggested change"),
                        )
                        .child(
                            div()
                                .px_2()
                                .py_1()
                                .rounded(theme.radius)
                                .border_1()
                                .border_color(theme.border)
                                .bg(theme.muted)
                                .font_family(mono.clone())
                                .text_size(theme.mono_font_size)
                                .child(s),
                        ),
                )
            })
            .when_some(
                match &c.status {
                    Status::Addressed { note, .. } => Some(note.clone()),
                    _ => None,
                },
                |d, note| {
                    d.child(
                        // One paragraph, so a narrow column wraps it like the body.
                        div()
                            .pt_1p5()
                            .border_t_1()
                            .border_color(theme.border)
                            .text_xs()
                            .whitespace_normal()
                            .text_color(theme.muted_foreground)
                            .child(StyledText::new(format!("Agent  {note}")).with_highlights([(
                                0..5,
                                HighlightStyle {
                                    font_weight: Some(FontWeight::SEMIBOLD),
                                    color: Some(theme.foreground),
                                    ..Default::default()
                                },
                            )])),
                    )
                },
            )
            .when_some(posted, |d, p| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("Posted as {p}. AI context is unchanged.")),
                )
            })
            .when(c.status != Status::Pending, |d| {
                // A narrow column wraps the buttons rather than clipping them.
                d.child(
                    h_flex()
                        .flex_wrap()
                        .gap_2()
                        .justify_end()
                        .when(in_context, |d| {
                            d.child(
                                Button::new(("show-chat", id as usize))
                                    .ghost()
                                    .xsmall()
                                    .flex_shrink_1()
                                    .min_w_0()
                                    .label("Show chat ↗")
                                    .on_click(cx.listener(move |this, _, w, cx| {
                                        this.show_thread_chat(id, w, cx)
                                    })),
                            )
                        })
                        .child(match c.github.clone() {
                            Some(url) => Button::new(("view-gh", id as usize))
                                .outline()
                                .xsmall()
                                .flex_shrink_1()
                                .min_w_0()
                                .label("View on GitHub ↗")
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                            None => Button::new(("share-gh", id as usize))
                                .outline()
                                .xsmall()
                                .flex_shrink_1()
                                .min_w_0()
                                .label("Share to GitHub…")
                                .on_click(
                                    cx.listener(move |this, _, w, cx| this.open_share(id, w, cx)),
                                ),
                        }),
                )
            })
            .test_support()
            .into_any_element()
    }

    /// A file header's review chip and Comment button: "● N pending", "Addressed", "Outdated";
    /// "Commenting" while the composer is in the file, else "Comment" / "N comments".
    pub(crate) fn review_chips(&self, path: &SharedString, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let threads: Vec<&Comment> = (self.ws_comments().iter())
            .filter(|c| c.shown() && c.path == path.as_str())
            .collect();
        let count = |f: &dyn Fn(&Status) -> bool| threads.iter().filter(|c| f(&c.status)).count();
        let pending = count(&|s| *s == Status::Pending);
        let chip = match () {
            _ if pending > 0 => Some(chip(
                format!("{pending} pending"),
                theme.warning_foreground,
                theme,
            )),
            _ if count(&|s| *s == Status::Outdated) > 0 => {
                Some(chip("Outdated".into(), theme.warning_foreground, theme))
            }
            _ if count(&|s| matches!(s, Status::Addressed { .. })) > 0 => {
                Some(chip("Addressed".into(), theme.success_foreground, theme))
            }
            _ if count(&|s| matches!(s, Status::Queued | Status::Sent { .. })) > 0 => {
                Some(chip("Sent".into(), theme.primary, theme))
            }
            _ => None,
        };
        let commenting =
            (self.review.composer.as_ref()).is_some_and(|c| c.draft.path == path.as_str());
        let label = match (commenting, threads.len()) {
            (true, _) => "Commenting".to_string(),
            (_, 0) => "Comment".into(),
            (_, 1) => "1 comment".into(),
            (_, n) => format!("{n} comments"),
        };
        let (path, button) = (path.clone(), commenting || chip.is_none());
        h_flex()
            .gap(px(10.))
            .children(chip)
            .when(button, |d| {
                d.child(
                    Button::new(SharedString::from(format!("comment:{path}")))
                        .outline()
                        .xsmall()
                        .h(px(28.))
                        .px(px(9.))
                        .rounded(px(6.))
                        .label(label)
                        .on_click(cx.listener(move |this, _, w, cx| this.comment_in(&path, w, cx))),
                )
            })
            .into_any_element()
    }

    /// The header's Comment: the selection when it is in `path`, else its first changed line.
    pub(crate) fn comment_in(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let here = self.picked_lines(cx).filter(|r| r.path() == path);
        let range = here.or_else(|| {
            let first = (self.review.hunks.get(path))
                .and_then(|h| h.iter().flatten().find(|l| l.0 != b' '))
                .map(|&(k, old, new)| match k {
                    b'-' => (DiffSide::Original, old),
                    _ => (DiffSide::Modified, new),
                })?;
            Some(DiffLineRange::new(
                path.to_string(),
                first.0,
                first.1,
                first.1,
            ))
        });
        let Some(range) = range else { return };
        self.diff
            .update(cx, |d, cx| d.set_selected_lines(Some(range.clone()), cx));
        self.comment_at(range, window, cx);
    }

    /// The sticky review bar at the bottom of the diff (A·01/02/04). `status`, when set,
    /// replaces its left text (in-place editing says what it is doing there). Without
    /// threads it is the key hint line.
    pub(crate) fn render_review_bar(
        &self,
        status: Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let shown: Vec<&Comment> = self.ws_comments().iter().filter(|c| c.shown()).collect();
        let count = |f: &dyn Fn(&Status) -> bool| shown.iter().filter(|c| f(&c.status)).count();
        let pending = count(&|s| *s == Status::Pending);
        let addressed = count(&|s| matches!(s, Status::Addressed { .. }));
        let outdated = count(&|s| *s == Status::Outdated);
        // 28 tall with just a hint, 40 with the review's buttons.
        let bar = h_flex()
            .flex_none()
            .px(px(16.))
            .gap_3()
            .border_t_1()
            .border_color(theme.border)
            .text_xs()
            .text_color(theme.muted_foreground);
        if let Some(s) = status {
            return bar.h(px(28.)).child(s).into_any_element();
        }
        // Without Vim mode the arrows move the cursor, not j / k.
        let jk = if self.preferences.vim {
            "j / k"
        } else {
            "↑ / ↓"
        };
        if shown.is_empty() {
            return bar
                .h(px(28.))
                .child(format!("s split / unified · c comment · Drag divider to resize · {jk} move through changes"))
                .into_any_element();
        }
        let (lead, color, summary) = match pending {
            0 => (
                "Review · sent".to_string(),
                theme.success_foreground,
                format!("{addressed} addressed · {outdated} outdated · 0 pending"),
            ),
            n => {
                // In file order already, so a file's threads sit together.
                let mut files: Vec<&str> = (shown.iter())
                    .filter(|c| c.status == Status::Pending)
                    .map(|c| c.path.rsplit('/').next().unwrap_or(&c.path))
                    .collect();
                files.dedup();
                let files = match files.len() {
                    1 | 2 => files.join(" · "),
                    n => format!("{n} files"),
                };
                (
                    format!("Review · {n} pending"),
                    theme.warning_foreground,
                    files,
                )
            }
        };
        // Narrow, the lead, the summary and the keys truncate; the actions stay whole.
        let text = |t: String| div().min_w_0().truncate().child(t);
        let button = |id: &'static str, label: String| Button::new(id).small().label(label);
        let bar = bar.h(px(40.));
        bar.child(
            h_flex()
                .min_w_0()
                .gap_1p5()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.foreground)
                .child(div().flex_none().size(px(6.)).rounded_full().bg(color))
                .child(div().min_w_0().truncate().child(lead)),
        )
        .child(text(summary))
        .child(text(format!("[c / ]c jump · {jk} change · v viewed")).text_color(faint(theme)))
        .child(div().flex_1())
        .when(outdated > 0, |d| {
            d.child(
                button("review-requeue", "Re-queue outdated".into())
                    .ghost()
                    .on_click(cx.listener(|this, _, _, cx| this.requeue_outdated(cx))),
            )
        })
        .when(addressed > 0, |d| {
            d.child(
                button("review-resolve", format!("Resolve {addressed} addressed"))
                    .on_click(cx.listener(|this, _, _, cx| this.resolve_addressed(cx))),
            )
        })
        .when(pending > 0, |d| {
            d.child(
                button("review-discard", "Discard…".into())
                    .ghost()
                    .on_click(cx.listener(|this, _, w, cx| this.discard_pending(w, cx))),
            )
            .child(
                button("review-send", "Send review… ⇧C".into())
                    .when(self.review.sheet.is_none(), |b| b.primary())
                    .on_click(cx.listener(|this, _, w, cx| this.open_sheet(w, cx))),
            )
        })
        .into_any_element()
    }
}

/// "a reviewer note to shark #128" from a GitHub URL (`…/shark/pull/128#issuecomment-1`);
/// anything else on the PR was a review.
pub(crate) fn posted_as(url: &str) -> String {
    let mut parts = url.split('/').rev();
    let tail = parts.next().unwrap_or_default();
    let (n, frag) = tail.split_once('#').unwrap_or((tail, ""));
    let repo = parts.nth(1).unwrap_or_default();
    let kind = match frag.starts_with("issuecomment") {
        true => "a reviewer note",
        false => "a PR review",
    };
    format!("{kind} to {repo} #{n}")
}
#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,4 +1,4 @@ fn x\n a\n-b\n-c\n+B\n d\n\\ No newline at end of file\n@@ -10 +10,2 @@\n x\n+y\n";

    fn spots(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| match &r.spot {
                Spot::File(p) => format!("F {p}"),
                Spot::Line(l) => format!("{:?} {}", l.side(), l.line()),
            })
            .collect()
    }

    #[::core::prelude::v1::test]
    fn rows_follow_the_diff_layout() {
        let hunks = parse_hunks(PATCH);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0][3], (b'+', 0, 2));
        assert_eq!(hunks[1], [(b' ', 10, 10), (b'+', 0, 11)]);
        let files = [
            ("r/f".into(), &hunks[..], false),
            ("r/g".into(), &[][..], false),
        ];
        let unified = rows(&files, false);
        assert_eq!(
            spots(&unified),
            [
                "F r/f",
                "Modified 1",
                "Original 2",
                "Original 3",
                "Modified 2",
                "Modified 3",
                "Modified 10",
                "Modified 11",
                "F r/g"
            ]
        );
        let split = rows(&files, true);
        assert_eq!(spots(&split)[2..4], ["Modified 2", "Original 3"]);
        assert_eq!(rows(&[("r/f".into(), &hunks[..], true)], false).len(), 1);

        // A refresh that dropped the line lands on the closest one above it.
        let gone = Spot::Line(DiffLinePosition::new("r/f", DiffSide::Modified, 7));
        assert_eq!(find(&unified, &gone), 5);
        assert_eq!(find(&unified, &Spot::File("r/x".into())), 0);
        // An original line on a context row, or paired beside a working line in split.
        let orig = |n| Spot::Line(DiffLinePosition::new("r/f", DiffSide::Original, n));
        assert_eq!(find(&unified, &orig(10)), 6);
        assert_eq!(find(&split, &orig(2)), 2);
        assert_eq!(next_change(&unified, 0, true), Some(2));
        assert_eq!(next_change(&unified, 2, true), Some(7));
        assert_eq!(next_change(&unified, 7, true), Some(2), "wraps");
        // Cursor at the end of a 9-row (11 high) diff on a 10-row screen: the top can't pass
        // the point where the end shows.
        assert_eq!(fit(&unified, 0, 8, 10.), 2);
        assert_eq!(fit(&unified, 5, 1, 10.), 0, "scrolloff above");
    }

    #[::core::prelude::v1::test]
    fn prompt_lists_places_ids_and_suggestions() {
        let c = |id, start, end, text: &str| Comment {
            id,
            path: "api/a.rs".into(),
            start,
            end,
            text: text.into(),
            ..Default::default()
        };
        let p = prompt(
            &[
                Comment {
                    snapshot: "let x = 1;".into(),
                    rev: "working tree".into(),
                    suggestion: Some("let y = 1;".into()),
                    ..c(1, (false, 12), (false, 12), "rename")
                },
                Comment {
                    rev: "0123456789abcdef".into(),
                    ..c(2, (true, 3), (true, 5), "keep\nthis")
                },
                c(3, (true, 7), (false, 8), "x"),
            ],
            "Run cargo test.",
        );
        assert!(
            p.contains("\n[c1] api/a.rs:12 · working tree\n```\nlet x = 1;\n```\n> rename\nSuggested change:\n```\nlet y = 1;\n```\n"),
            "{p}"
        );
        assert!(
            p.contains("\n[c2] api/a.rs:3-5 (old) · base 0123456789\n> keep\n> this\n"),
            "{p}"
        );
        assert!(p.contains("\n[c3] api/a.rs:7 (old)-8 (new)\n"));
        assert!(p.contains("\nOverall: Run cargo test.\n"));
        assert!(p.contains("[c<id>] addressed: "));
        assert!(!prompt(&[], "  ").contains("Overall"));
        assert_eq!(fenced("a ``` b", "x"), "````x\na ``` b\n````");
    }

    /// An old-side comment without a base reads nothing, not the index (`git show :path`).
    #[::core::prelude::v1::test]
    fn old_lines_without_a_base() {
        let dir = std::env::temp_dir().join(format!("shark-rev-{}", std::process::id()));
        let repo = dir.join("api");
        std::fs::create_dir_all(&repo).unwrap();
        crate::git::git(&repo, &["init", "-q"]).unwrap();
        std::fs::write(repo.join("f"), "staged\n").unwrap();
        crate::git::git(&repo, &["add", "f"]).unwrap();
        let mut c = Comment {
            path: "api/f".into(),
            start: (true, 1),
            end: (true, 1),
            ..Default::default()
        };
        assert_eq!(lines_now(&dir, &mut c), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A working-copy thread follows its lines through edits above it. Its own line edited,
    /// it stays, so it reads Outdated, though the same text is further down; never read, it
    /// only goes to a snapshot found once.
    #[::core::prelude::v1::test]
    fn reanchors_through_the_diff() {
        let file = |ten: &str, top: &str| {
            let mut l: Vec<String> = (1..=20).map(|i| format!("l{i}")).collect();
            (l[9], l[19]) = (ten.into(), "return None;".into());
            format!("{top}{}\n", l.join("\n"))
        };
        let mut c = Comment {
            start: (false, 10),
            end: (false, 10),
            snapshot: "return None;".into(),
            ..Default::default()
        };
        reanchor(&mut c, &file("return None;", ""));
        assert_eq!((c.start, c.end), ((false, 10), (false, 10)));
        reanchor(&mut c, &file("return None;", "a\nb\n"));
        assert_eq!((c.start, c.end), ((false, 12), (false, 12)));
        let edited = file("return Some(1);", "a\nb\n");
        reanchor(&mut c, &edited);
        assert_eq!((c.start, c.end), ((false, 12), (false, 12)));
        let now = slice(&edited, c.start.1, c.end.1);
        let sent = Status::Sent { turn: 1 };
        assert_eq!(
            judge(&sent, 1, None, Some(&now), &c.snapshot),
            Status::Outdated
        );
        reanchor(&mut c, "gone\n");
        assert_eq!((c.start, c.end), ((false, 12), (false, 12)));

        let mut d = Comment {
            start: (false, 2),
            end: (false, 3),
            snapshot: "l2\nl3".into(),
            ..Default::default()
        };
        reanchor(&mut d, &file("x", "a\n"));
        assert_eq!((d.start, d.end), ((false, 3), (false, 4)));
    }

    #[::core::prelude::v1::test]
    fn reports_and_judging() {
        // The prompt's own instruction line, quoted back, is no report.
        let reply = "Done.\n\n[c1] addressed: Ratio stored once.\n- `[c12] Addressed: renamed`\n[c3] skipped: no\n[cx] addressed: bad\n[c<id>] addressed: <what you changed, in one line>\n";
        assert_eq!(
            parse_reports(reply),
            [
                (1, "Ratio stored once.".to_string()),
                (12, "renamed".into())
            ]
        );
        let sent = Status::Sent { turn: 2 };
        assert_eq!(
            judge(&sent, 3, Some("done"), Some("new"), "old"),
            Status::Addressed {
                turn: 3,
                note: "done".into()
            }
        );
        assert_eq!(judge(&sent, 3, None, Some("new"), "old"), Status::Outdated);
        assert_eq!(judge(&sent, 3, None, Some("old"), "old"), sent);
        assert_eq!(judge(&sent, 3, None, None, "old"), sent, "unknown lines");
        assert_eq!(judge(&sent, 3, None, Some("x"), ""), sent, "no snapshot");
        assert_eq!(
            judge(&Status::Pending, 3, Some("done"), None, ""),
            Status::Pending
        );
        assert_eq!(slice("a\nb\nc\nd", 2, 3), "b\nc");
        assert_eq!(slice("a", 4, 5), "");
        assert_eq!(
            posted_as("https://github.com/o/shark/pull/128#issuecomment-9"),
            "a reviewer note to shark #128"
        );
        assert_eq!(
            posted_as("https://github.com/o/shark/pull/7"),
            "a PR review to shark #7"
        );
    }

    /// comments/<ws>.json from before ids and statuses still loads; each gets its own id.
    #[::core::prelude::v1::test]
    fn old_comment_files_load() {
        let old = r#"[{"path":"api/a","start":[false,1],"end":[false,2],"text":"one"},
                      {"path":"api/b","start":[true,3],"end":[true,3],"text":"two"}]"#;
        let c: Vec<Comment> = serde_json::from_str(old).unwrap();
        assert_eq!(
            (c[0].id, &c[0].status, c[1].start),
            (0, &Status::Pending, (true, 3))
        );
        let mut c = c;
        let mut last = 0;
        assert!(give_ids(&mut c, &mut last));
        assert_eq!(c.iter().map(|c| c.id).collect::<Vec<_>>(), [1, 2]);
        assert!(!give_ids(&mut c, &mut last), "only once");
        assert_eq!(last, 2);
        // The list alone (older files) and with the last id both load.
        let Ok(Store::List(l)) = serde_json::from_str(old) else {
            panic!("old list")
        };
        assert_eq!(l.len(), 2);
        let new = r#"{"last":9,"comments":[{"id":3,"path":"api/a","start":[false,1],"end":[false,1],"text":"x"}]}"#;
        let Ok(Store::Ids { last, comments }) = serde_json::from_str(new) else {
            panic!("new store")
        };
        assert_eq!((last, comments[0].id), (9, 3));
        // A comment queued in a chat that is gone is pending again; others stay.
        let mut l = vec![
            Comment {
                id: 1,
                status: Status::Queued,
                chat: Some("a".into()),
                ..Default::default()
            },
            Comment {
                id: 2,
                status: Status::Queued,
                chat: Some("b".into()),
                ..Default::default()
            },
            Comment {
                id: 3,
                status: Status::Sent { turn: 1 },
                chat: Some("a".into()),
                ..Default::default()
            },
        ];
        assert!(unqueue(&mut l, Some("a"), &[]));
        assert_eq!((&l[0].status, &l[0].chat), (&Status::Pending, &None));
        assert_eq!(l[1].status, Status::Queued);
        assert_eq!(
            l[2].status,
            Status::Sent { turn: 1 },
            "read before it closed"
        );
        // Closed before reading it: pending again.
        l[2].status = Status::Sent { turn: 1 };
        assert!(unqueue(&mut l, Some("a"), &[3]));
        assert_eq!((&l[2].status, &l[2].chat), (&Status::Pending, &None));
        // After a restart: sent to a chat that is gone (no transcript), pending again.
        l[2].status = Status::Sent { turn: 1 };
        l[2].chat = Some("no-such-chat-r3".into());
        assert!(unqueue(&mut l, None, &[]) && !unqueue(&mut l, None, &[]));
        assert_eq!(
            (&l[1].status, &l[2].status),
            (&Status::Pending, &Status::Pending)
        );
        let s = Status::Addressed {
            turn: 5,
            note: "n".into(),
        };
        let back: Status = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }
}
