//! A review comment: its model, the prompt it becomes, and following its lines through edits.
use super::*;
use crate::diff_view::{DiffLinePosition, DiffLineRange, DiffSide};
use serde::{Deserialize, Serialize};

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
    /// In the AI's context: queued or sent, and what became of it.
    pub(super) fn in_context(&self) -> bool {
        matches!(
            self,
            Status::Queued | Status::Sent { .. } | Status::Addressed { .. } | Status::Outdated
        )
    }

    /// Resolvable once sent; a pending or queued thread would vanish unsent.
    pub(super) fn resolvable(&self) -> bool {
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
    pub(super) start: (bool, usize),
    pub(super) end: (bool, usize),
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
    pub(super) seen: Seen,
}

/// Memory only, and not part of a comment's equality (that decides saving).
#[derive(Clone, Debug, Default)]
pub(super) struct Seen(Option<String>);

impl PartialEq for Seen {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Comment {
    pub(super) fn from_range(r: &DiffLineRange) -> Self {
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
        let s = crate::ui::plural(self.start.1.abs_diff(self.end.1) + 1);
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

    pub(super) fn anchor(&self) -> DiffLinePosition {
        DiffLinePosition::new(self.path.clone(), side(self.end.0), self.end.1)
    }

    pub(super) fn shown(&self) -> bool {
        self.status != Status::Resolved
    }

    /// Whether line `l` of the diff falls inside this comment.
    pub(super) fn covers(&self, l: &DiffLinePosition) -> bool {
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

pub(super) fn side(old: bool) -> DiffSide {
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
pub(super) fn slice(src: &str, start: usize, end: usize) -> String {
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
pub(super) fn lines_now(dir: &Path, c: &mut Comment) -> Option<String> {
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
            let o = crate::git::git_bytes(&dir.join(repo), &["show", &format!("{}:{rel}", c.rev)]);
            String::from_utf8_lossy(&o.ok()?).into_owned()
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
    use crate::chat::parse_reports;

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
}
