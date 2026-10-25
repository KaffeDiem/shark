//! Share to GitHub (07–08): one thread to its repo's PR as a reviewer note (`gh pr comment`)
//! or a PR review (`gh pr review --comment`), previewed first. The AI context is untouched;
//! a failed post keeps the comment and the dialog, to retry.
use super::review::{Comment, fence};
use super::*;
use crate::sync::Pr;
use gpui_kit::component::radio::{Radio, RadioGroup};

pub(super) struct Share {
    pub(super) id: u64,
    /// A PR review instead of a reviewer note.
    pub(super) review: bool,
    pub(super) posting: bool,
    pub(super) error: Option<String>,
}

/// `s` without the indentation all its lines share, so an excerpt reads from the left.
fn dedent(s: &str) -> String {
    let lines = s.lines().filter(|l| !l.trim().is_empty());
    let n = lines
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    (s.lines().map(|l| l.get(n..).unwrap_or("")))
        .collect::<Vec<_>>()
        .join("\n")
}

/// What GitHub receives for `comments`: path, side and range, the code as commented, the
/// text. No inline anchor: the lines may not be pushed.
pub(super) fn github_body(comments: &[Comment], note: &str) -> String {
    let mut out = String::new();
    for c in comments {
        out += &format!("**{}** · {} · {}\n\n", c.path, c.side(), c.lines());
        if !c.snapshot.is_empty() {
            let f = fence(&c.snapshot);
            out += &format!("{f}\n{}\n{f}\n\n", c.snapshot);
        }
        out += &format!("{}\n\n", c.text);
        if let Some(s) = &c.suggestion {
            let f = fence(s);
            out += &format!("Suggested change:\n{f}\n{s}\n{f}\n\n");
        }
    }
    if !note.trim().is_empty() {
        out += &format!("{}\n", note.trim());
    }
    out.trim_end().to_string()
}

/// An inline comment's text: GitHub shows a suggestion it can apply.
fn inline_body(c: &Comment) -> String {
    match &c.suggestion {
        Some(s) => {
            let f = fence(s);
            format!("{}\n\n{f}suggestion\n{s}\n{f}", c.text)
        }
        None => c.text.clone(),
    }
}

impl Shark {
    /// The open PR of `c`'s repo, and whether the lines are on GitHub (head pushed, clean).
    pub(super) fn pr_of(&self, c: &Comment, cx: &App) -> Option<(Pr, bool)> {
        let repo = c.path.split('/').next()?;
        self.active_sync()?.read(cx).pr_for(repo)
    }

    /// "Share to GitHub…" on a thread: the dialog (07) over the window.
    pub(super) fn open_share(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.comment(id) else { return };
        if self.pr_of(c, cx).is_none() {
            let repo = c.path.split('/').next().unwrap_or_default().to_string();
            return notify_err(window, cx, format!("{repo} has no open PR to share to"));
        }
        self.review.share = Some(Share {
            id,
            review: false,
            posting: false,
            error: None,
        });
        let shark = cx.entity().downgrade();
        window.open_dialog(cx, move |d, _, cx| match shark.upgrade() {
            Some(s) => s.update(cx, |s, cx| s.share_dialog(d, cx)),
            None => d,
        });
    }

    /// Closes the dialog, if it is open.
    pub(super) fn close_share(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.review.share.take().is_some() {
            window.close_dialog(cx);
        }
        cx.notify();
    }

    /// Posts the dialog's thread; on success the thread keeps the URL.
    pub(super) fn post_share(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = &self.review.share else { return };
        if s.posting {
            return;
        }
        let (id, review) = (s.id, s.review);
        let Some(c) = self.comment(id).cloned() else {
            return;
        };
        self.publish(&[c], "", review, window, cx);
    }

    /// Posts `comments` (with `note`) to their repos' PRs in the background, one post per
    /// repo: a review, else a reviewer note. Each gets its URL; a failure, or a repo without
    /// a PR, says so and leaves its comments as they were.
    pub(super) fn publish(
        &mut self,
        comments: &[Comment],
        note: &str,
        review: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut repos: Vec<&str> = vec![];
        for c in comments {
            let repo = c.path.split('/').next().unwrap_or_default();
            if !repos.contains(&repo) {
                repos.push(repo);
            }
        }
        for repo in repos {
            let group: Vec<Comment> = (comments.iter())
                .filter(|c| c.path.split('/').next() == Some(repo))
                .cloned()
                .collect();
            self.publish_repo(&group, note, review, window, cx);
        }
    }

    /// The dialog is about one of `ids`, in workspace `ws`: it shows their post.
    fn shares(&self, ws: &str, ids: &[u64]) -> bool {
        self.active.as_deref() == Some(ws)
            && (self.review.share.as_ref()).is_some_and(|sh| ids.contains(&sh.id))
    }

    /// `publish` for comments of one repo.
    fn publish_repo(
        &mut self,
        comments: &[Comment],
        note: &str,
        review: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(first), Some(ws)) = (comments.first(), self.active_ws()) else {
            return;
        };
        let repo = first.path.split('/').next().unwrap_or_default();
        let Some((pr, pushed)) = self.pr_of(first, cx) else {
            return notify_err(window, cx, format!("{repo} has no open PR to publish to"));
        };
        let (wt, ws_name) = (ws.dir().join(repo), ws.name.clone());
        // A review of pushed lines sits on them; anything else goes as path and excerpt.
        // ponytail: one thread at a time; the sheet's multi-comment review stays text.
        let inline = (review && pushed && comments.len() == 1 && self.on_its_line(first))
            .then(|| Some((first.gh_anchor()?, inline_body(first))))
            .flatten();
        // GitHub refuses a Comment review without a body: the note, else where it is.
        let body = match inline {
            Some(_) if note.trim().is_empty() => first.place(),
            Some(_) => note.trim().to_string(),
            None => github_body(comments, note),
        };
        let ids: Vec<u64> = comments.iter().map(|c| c.id).collect();
        if self.shares(&ws_name, &ids)
            && let Some(s) = &mut self.review.share
        {
            s.posting = true;
            s.error = None;
        }
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let res = (cx.background_executor())
                .spawn(async move {
                    let inline = inline.as_ref().map(|(a, t)| (a, t.as_str()));
                    match review {
                        true => crate::pr::publish_review(&wt, pr.number, &body, inline)
                            .map(|u| if u.is_empty() { pr.url } else { u }),
                        false => crate::pr::publish_note(&wt, pr.number, &body),
                    }
                })
                .await;
            this.update_in(cx, |s, window, cx| {
                let ours = s.shares(&ws_name, &ids);
                match res {
                    Ok(url) => {
                        // Into the workspace they came from, even if another one is open now.
                        s.review.edit(&ws_name, |l| {
                            for c in l.iter_mut().filter(|c| ids.contains(&c.id)) {
                                c.github = Some(url.clone());
                            }
                        });
                        if ours {
                            s.close_share(window, cx);
                        }
                        window.push_notification("Posted to GitHub", cx);
                    }
                    Err(e) => {
                        if ours && let Some(sh) = &mut s.review.share {
                            sh.posting = false;
                            sh.error = Some(e.clone());
                        }
                        notify_err(
                            window,
                            cx,
                            format!("GitHub post failed; the comment is kept: {e}"),
                        );
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The Share to GitHub dialog's content (07); Enter posts, Esc closes.
    fn share_dialog(&self, d: Dialog, cx: &mut Context<Self>) -> Dialog {
        let Some(s) = &self.review.share else {
            return d;
        };
        let (Some(c), Some(ws)) = (self.comment(s.id), self.active_ws()) else {
            return d;
        };
        let Some((pr, pushed)) = self.pr_of(c, cx) else {
            return d;
        };
        let theme = cx.theme();
        let repo = c.path.split('/').next().unwrap_or_default();
        let base = (ws.repos.iter().find(|r| r.name == repo))
            .map(|r| r.base.trim_start_matches("origin/").to_string())
            .unwrap_or_default();
        let this = cx.entity().downgrade();
        let gone = this.clone();
        d.font_features(inter_features())
            .entrance(DialogEntrance::None)
            .title("Share to GitHub")
            .w(px(440.))
            .on_ok(move |_, w, cx| {
                this.update(cx, |s, cx| s.post_share(w, cx)).ok();
                false // closed when the post lands
            })
            .on_close(move |_, _, cx| {
                gone.update(cx, |s, cx| {
                    s.review.share = None;
                    cx.notify();
                })
                .ok();
            })
            .child(
                v_flex()
                    .gap_3()
                    .text_sm()
                    .child(
                        v_flex()
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(format!("{repo} · PR #{}", pr.number)),
                            )
                            .child(div().text_xs().text_color(theme.muted_foreground).child(
                                format!(
                                    "{} → {base} · Visible to everyone with PR access",
                                    ws.branch
                                ),
                            )),
                    )
                    .child(
                        RadioGroup::horizontal("share-kind")
                            .child(Radio::new("share-note").label("Reviewer note"))
                            .child(Radio::new("share-review").label("PR review"))
                            .selected_index(Some(s.review as usize))
                            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                if let Some(s) = &mut this.review.share {
                                    s.review = *ix == 1;
                                }
                                cx.notify();
                            })),
                    )
                    .child(div().text_xs().text_color(theme.muted_foreground).child(match s.review {
                        false => "Posts this comment to the PR conversation as a note for the reviewer. Your AI chat stays private.",
                        true => "Posts this comment as a Comment review on the PR, without approving or requesting changes. Your AI chat stays private.",
                    }))
                    .child(
                        v_flex()
                            .p_3()
                            .gap_2()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.border)
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(c.card_place()),
                            )
                            .when(!c.snapshot.is_empty(), |d| {
                                d.child(
                                    div()
                                        .font_family(theme.mono_font_family.clone())
                                        .text_size(theme.mono_font_size)
                                        .child(dedent(&c.snapshot)),
                                )
                            })
                            .child(div().whitespace_normal().child(c.text.clone())),
                    )
                    .when(!pushed, |d| {
                        d.child(div().text_xs().text_color(theme.warning_foreground).child(
                            "These lines are not pushed yet. GitHub will receive the file path and code excerpt, without an inline anchor.",
                        ))
                    })
                    .when_some(s.error.clone(), |d, e| {
                        d.child(div().text_xs().text_color(theme.danger_foreground).child(e))
                    }),
            )
            .footer(
                h_flex()
                    .w_full()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("share-cancel")
                            .ghost()
                            .small()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, w, cx| this.close_share(w, cx))),
                    )
                    .child(
                        Button::new("share-post")
                            .primary()
                            .small()
                            .loading(s.posting)
                            .label(match (s.error.is_some(), s.review) {
                                (true, _) => "Retry",
                                (false, false) => "Post note to GitHub",
                                (false, true) => "Post review",
                            })
                            .on_click(cx.listener(|this, _, w, cx| this.post_share(w, cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin

    #[test]
    fn github_body_has_place_code_and_text() {
        let c: Comment = serde_json::from_str(
            r#"{"path":"shark/src/app.rs","start":[false,631],"end":[false,632],"text":"Keep it aligned.","snapshot":".size(px(400.))"}"#,
        )
        .unwrap();
        assert_eq!(
            github_body(&[c], " Thanks "),
            "**shark/src/app.rs** · working copy · 631–632\n\n```\n.size(px(400.))\n```\n\nKeep it aligned.\n\nThanks"
        );
        // Each block gets a fence longer than the backticks in it, the suggestion its own.
        let c: Comment = serde_json::from_str(
            r#"{"path":"a/m.md","start":[false,1],"end":[false,1],"text":"t","snapshot":"````x","suggestion":"```y"}"#,
        )
        .unwrap();
        let body = github_body(std::slice::from_ref(&c), "");
        assert!(body.contains("\n`````\n````x\n`````\n"), "{body}");
        assert!(
            body.ends_with("Suggested change:\n````\n```y\n````"),
            "{body}"
        );
        assert_eq!(inline_body(&c), "t\n\n````suggestion\n```y\n````");
    }
}
