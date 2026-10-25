//! Share to GitHub (07–08): one thread to its repo's PR as a reviewer note (`gh pr comment`)
//! or a PR review (`gh pr review --comment`), previewed first. The AI context is untouched;
//! a failed post keeps the comment and the dialog, to retry.
use super::review::Comment;
use super::*;
use crate::sync::Pr;

pub(super) struct Share {
    pub(super) id: u64,
    /// A PR review instead of a reviewer note.
    pub(super) review: bool,
    pub(super) posting: bool,
    pub(super) error: Option<String>,
    pub(super) focus: FocusHandle,
}

/// What GitHub receives for `comments`: path, side and range, the code as commented, the
/// text. No inline anchor: the lines may not be pushed.
pub(super) fn github_body(comments: &[Comment], note: &str) -> String {
    let mut out = String::new();
    for c in comments {
        let f = if c.snapshot.contains("```") {
            "````"
        } else {
            "```"
        };
        out += &format!("**{}** · {} · {}\n\n", c.path, c.side(), c.lines());
        if !c.snapshot.is_empty() {
            out += &format!("{f}\n{}\n{f}\n\n", c.snapshot);
        }
        out += &format!("{}\n\n", c.text);
        if let Some(s) = &c.suggestion {
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
        Some(s) => format!("{}\n\n```suggestion\n{s}\n```", c.text),
        None => c.text.clone(),
    }
}

impl Shark {
    /// The open PR of `c`'s repo, and whether the lines are on GitHub (head pushed, clean).
    pub(super) fn pr_of(&self, c: &Comment, cx: &App) -> Option<(Pr, bool)> {
        let repo = c.path.split('/').next()?;
        self.active_sync()?.read(cx).pr_for(repo)
    }

    /// "Share to GitHub…" on a thread.
    pub(super) fn open_share(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.comment(id) else { return };
        if self.pr_of(c, cx).is_none() {
            let repo = c.path.split('/').next().unwrap_or_default().to_string();
            return notify_err(window, cx, format!("{repo} has no open PR to share to"));
        }
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        self.review.share = Some(Share {
            id,
            review: false,
            posting: false,
            error: None,
            focus,
        });
        cx.notify();
    }

    pub(super) fn close_share(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.review.share = None;
        self.diff.read(cx).focus_handle(cx).focus(window, cx);
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

    /// Posts `comments` (with `note`) to their PR in the background: a review, else a reviewer
    /// note. Each gets the URL; a failure says so and leaves them as they were.
    pub(super) fn publish(
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
        let Some((pr, pushed)) = self.pr_of(first, cx) else {
            return notify_err(window, cx, "No open PR to publish to");
        };
        let repo = first.path.split('/').next().unwrap_or_default();
        let wt = ws.dir().join(repo);
        // A review of pushed lines sits on them; anything else goes as path and excerpt.
        // ponytail: one thread at a time; the sheet's multi-comment review stays text.
        let inline = (review && pushed && comments.len() == 1 && self.on_its_line(first))
            .then(|| Some((first.gh_anchor()?, inline_body(first))))
            .flatten();
        let body = match inline {
            Some(_) => note.trim().to_string(),
            None => github_body(comments, note),
        };
        let ids: Vec<u64> = comments.iter().map(|c| c.id).collect();
        if let Some(s) = &mut self.review.share {
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
            this.update_in(cx, |s, window, cx| match res {
                Ok(url) => {
                    s.edit_comments(cx, |l| {
                        for c in l.iter_mut().filter(|c| ids.contains(&c.id)) {
                            c.github = Some(url.clone());
                        }
                    });
                    if s.review
                        .share
                        .as_ref()
                        .is_some_and(|sh| ids.contains(&sh.id))
                    {
                        s.close_share(window, cx);
                    }
                    window.push_notification("Posted to GitHub", cx);
                }
                Err(e) => {
                    if let Some(sh) = &mut s.review.share {
                        sh.posting = false;
                        sh.error = Some(e.clone());
                    }
                    notify_err(
                        window,
                        cx,
                        format!("GitHub post failed; the comment is kept: {e}"),
                    );
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The Share to GitHub dialog (07), over the diff.
    pub(super) fn render_share(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let s = self.review.share.as_ref()?;
        let c = self.comment(s.id)?;
        let (pr, pushed) = self.pr_of(c, cx)?;
        let theme = cx.theme();
        let repo = c.path.split('/').next().unwrap_or_default().to_string();
        let ws = self.active_ws()?;
        let base = (ws.repos.iter().find(|r| r.name == repo))
            .map(|r| r.base.trim_start_matches("origin/").to_string())
            .unwrap_or_default();
        let radio = |id: &'static str, label: &'static str, on: bool, review: bool| {
            h_flex()
                .id(id)
                .flex_1()
                .h(px(36.))
                .px_3()
                .gap_2()
                .rounded(theme.radius)
                .border_1()
                .border_color(if on { theme.primary } else { theme.border })
                .cursor_pointer()
                .child(
                    div()
                        .size(px(12.))
                        .rounded_full()
                        .border_1()
                        .border_color(if on {
                            theme.primary
                        } else {
                            theme.muted_foreground
                        })
                        .when(on, |d| d.border_3()),
                )
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(s) = &mut this.review.share {
                        s.review = review;
                    }
                    cx.notify();
                }))
        };
        let card = v_flex()
            .id("share-dialog")
            .key_context("ShareDialog")
            .track_focus(&s.focus)
            .on_action(cx.listener(|this, _: &CancelEdit, w, cx| this.close_share(w, cx)))
            .on_action(cx.listener(|this, _: &SubmitForm, w, cx| this.post_share(w, cx)))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .w(px(440.))
            .p_5()
            .gap_3()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .text_sm()
            .child(
                h_flex()
                    .child(div().flex_1().text_lg().font_weight(FontWeight::SEMIBOLD).child("Share to GitHub"))
                    .child(
                        Button::new("share-close")
                            .ghost()
                            .xsmall()
                            .icon(IconName::X)
                            .on_click(cx.listener(|this, _, w, cx| this.close_share(w, cx))),
                    ),
            )
            .child(
                v_flex()
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(format!("{repo} · PR #{}", pr.number)),
                    )
                    .child(div().text_xs().text_color(theme.muted_foreground).child(format!(
                        "{} → {base} · Visible to everyone with PR access",
                        ws.branch
                    ))),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(radio("share-note", "Reviewer note", !s.review, false))
                    .child(radio("share-review", "PR review", s.review, true)),
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
                    .child(div().text_xs().text_color(theme.muted_foreground).child(format!(
                        "{} · {} · {}",
                        c.path.rsplit('/').next().unwrap_or(&c.path),
                        if c.side() == "original" { "Original" } else { "Working copy" },
                        c.lines()
                    )))
                    .when(!c.snapshot.is_empty(), |d| {
                        d.child(
                            div()
                                .font_family(theme.mono_font_family.clone())
                                .text_size(theme.mono_font_size)
                                .child(c.snapshot.clone()),
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
            })
            .child(
                h_flex()
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
            );
        Some(
            div()
                .id("share-overlay")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme.overlay)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, w, cx| this.close_share(w, cx)),
                )
                .child(card)
                .into_any_element(),
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
    }
}
