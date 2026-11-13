//! Review prompts, cards, and addressed-comment reports shared by chat and diff review.

/// Diff comments sent as one message (src/features/differ/review.rs), shown as a card instead of the
/// prompt text.
#[derive(Clone)]
pub struct ReviewCard {
    pub prompt: String,
    pub items: Vec<ReviewItem>,
    /// The diff when it was sent, e.g. "3 files · +5 −3".
    pub changes: String,
}

impl ReviewCard {
    pub fn ids(&self) -> Vec<u64> {
        self.items.iter().map(|i| i.id).collect()
    }
}

/// A `[c<id>] addressed: <note>` line, as `(id, note)`.
pub(crate) fn report(l: &str) -> Option<(u64, String)> {
    let l = l.trim().trim_start_matches(['-', '*', '`', ' ']);
    let (id, rest) = l.strip_prefix("[c")?.split_once(']')?;
    let rest = rest.trim_start();
    let note = rest
        .get(..10)
        .filter(|p| p.eq_ignore_ascii_case("addressed:"))
        .map(|_| &rest[10..])?;
    Some((
        id.parse().ok()?,
        note.trim().trim_end_matches('`').trim().to_string(),
    ))
}

/// The reply's lines (with their ends), each with its report when it is one; lines in a code
/// block are none.
fn report_lines(reply: &str) -> impl Iterator<Item = (&str, Option<(u64, String)>)> {
    let mut fence: Option<&str> = None;
    reply.split_inclusive('\n').map(move |l| {
        let ticks = l.trim_start();
        let ticks = &ticks[..ticks.len() - ticks.trim_start_matches('`').len()];
        let inside = fence.is_some();
        match fence {
            None if ticks.len() >= 3 => fence = Some(ticks),
            Some(f) if ticks.starts_with(f) && l.trim().len() == ticks.len() => fence = None,
            _ => {}
        }
        (l, (!inside && fence.is_none()).then(|| report(l)).flatten())
    })
}

/// The reply's `[c<id>] addressed: <note>` lines, as `(id, note)`.
pub fn parse_reports(reply: &str) -> Vec<(u64, String)> {
    report_lines(reply).filter_map(|(_, r)| r).collect()
}

/// The reply as shown: without its `[c<id>] addressed:` lines, which only feed the diff's
/// threads (src/features/differ/review.rs).
pub(crate) fn hide_reports(reply: &str) -> String {
    (report_lines(reply).filter(|(_, r)| r.is_none()))
        .map(|(l, _)| l)
        .collect()
}

/// A reply that is only report lines: nothing to show. Stops at the first other line, so
/// it is cheap per frame.
pub(crate) fn hidden_reply(src: &str) -> bool {
    let mut any = false;
    for (l, r) in report_lines(src) {
        match r {
            Some(_) => any = true,
            None if l.trim().is_empty() => {}
            None => return false,
        }
    }
    any
}

/// Some line of `text` starts like a report (`[c<digit>`), unlike `#[cfg(...)]`.
pub(crate) fn may_report(text: &str) -> bool {
    text.lines().any(|l| {
        let l = l.trim_start().trim_start_matches(['-', '*', '`', ' ']);
        l.strip_prefix("[c")
            .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()))
    })
}

/// A review's prompt (review.rs `prompt`) back as its card, for a replayed transcript: each
/// `[c<id>] <place>` and its quoted text. The diff it was sent with is unknown by then.
pub(crate) fn card_of(prompt: &str) -> Option<ReviewCard> {
    // A task note may come before it on the wire.
    let prompt = &prompt[prompt.find("Review comments on the diff.")?..];
    let (mut items, mut fence) = (Vec::<ReviewItem>::new(), None);
    for l in prompt.lines() {
        if let Some(f) = fence {
            fence = (l != f).then_some(f);
            continue;
        }
        if l.starts_with("```") {
            fence = Some(l);
        } else if let Some((id, rest)) = l.strip_prefix("[c").and_then(|l| l.split_once("] "))
            && let Ok(id) = id.parse()
        {
            let place = rest.split(" · ").next().unwrap_or(rest).to_string();
            items.push(ReviewItem {
                id,
                place,
                text: String::new(),
            });
        } else if let (Some(t), Some(it)) = (l.strip_prefix("> "), items.last_mut()) {
            if !it.text.is_empty() {
                it.text.push('\n');
            }
            it.text.push_str(t);
        }
    }
    (!items.is_empty()).then(|| ReviewCard {
        prompt: prompt.to_string(),
        items,
        changes: String::new(),
    })
}

/// One comment of a review card: its id, "app.rs · Working copy · lines 631–632", its text.
#[derive(Clone)]
pub struct ReviewItem {
    pub id: u64,
    pub place: String,
    pub text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A review prompt in a replayed transcript comes back as its card; report lines stay
    /// out of the shown reply.
    #[test]
    fn review_cards_and_reports() {
        let p = "note\n\nReview comments on the diff. Paths …:\n\n[c1] api/a.rs:12 · working tree\n````\n[c9] x\n> y\n````\n> rename\n> it\nSuggested change:\n```\nlet y;\n```\n\n[c4] api/b.rs:3-5 (old) · base 0123456789\n> keep\n\nEnd your reply with one line per comment you addressed, exactly like:\n[c<id>] addressed: <what you changed, in one line>\n";
        let card = card_of(p).unwrap();
        let items: Vec<_> = (card.items.iter())
            .map(|i| (i.id, i.place.as_str(), i.text.as_str()))
            .collect();
        assert_eq!(
            items,
            [
                (1, "api/a.rs:12", "rename\nit"),
                (4, "api/b.rs:3-5 (old)", "keep")
            ]
        );
        assert!(card.prompt.starts_with("Review comments"));
        assert!(card_of("plain [c1] x\n> y").is_none());
        assert_eq!(
            hide_reports("Done.\n[c1] addressed: x\n- `[c2] addressed: y`\nBye"),
            "Done.\nBye"
        );
        // One in a code block is an example, not a report.
        let fenced = "See:\n```\n[c1] addressed: example\n```\n[c2] addressed: real";
        assert_eq!(parse_reports(fenced), [(2, "real".to_string())]);
        assert_eq!(
            hide_reports(fenced),
            "See:\n```\n[c1] addressed: example\n```\n"
        );
        assert!(hidden_reply("[c1] addressed: x\n"));
        assert!(hidden_reply("\n- [c1] addressed: x\n[c2] addressed: y"));
        assert!(!hidden_reply("Done.\n[c1] addressed: x"));
        assert!(!hidden_reply("") && !hidden_reply("#[cfg(test)]"));
        assert!(may_report("x\n - `[c12] addr") && may_report("[c3"));
        assert!(!may_report("#[cfg(test)]\n[cx] y\n[c"));
    }
}
