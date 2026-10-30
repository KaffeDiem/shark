//! Plan mode: the plans a chat's agent submits with `ExitPlanMode`, one version each, and the
//! pane that shows a version as markdown with its changes from the one before redlined in.
use crate::state::support_dir;
use crate::*;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::text::TextViewState;
use gpui_kit::component::{ActiveTheme, Icon, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde::{Deserialize, Serialize};
use std::ops::Range;
use std::path::PathBuf;

mod redline;
pub use redline::{Redline, redline};

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

/// Where a chat's plans are saved.
pub(crate) fn store_path(chat: &str) -> PathBuf {
    support_dir().join("plans").join(format!("{chat}.json"))
}

fn load(chat: &str) -> Store {
    std::fs::read(store_path(chat))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// The chat's latest plan is still waiting for the user's approval.
pub fn unapproved(chat: &str) -> bool {
    let s = load(chat);
    !s.versions.is_empty() && s.approved != Some(s.versions.len() - 1)
}

/// The user approved one of the chat's plans.
pub fn approved_any(chat: &str) -> bool {
    load(chat).approved.is_some()
}

/// With the chat's transcript: a later chat with the same id starts with no plans.
pub fn delete(chat: &str) {
    let _ = std::fs::remove_file(store_path(chat));
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
        let store = load(chat);
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
        // Written aside, synced and renamed, so a failed write never truncates the history.
        if let Ok(json) = serde_json::to_vec(&store) {
            let _ = crate::state::write(&store_path(&self.chat), &json);
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
            crate::ui::copy_with(v.text.clone(), "Copied the plan", window, cx);
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
                ..Default::default()
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
        let (muted, faint) = (theme.muted_foreground, crate::ui::faint(theme));
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
                        .child(crate::ui::kbd("shift-tab"))
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
                .child(crate::chat::markdown(&self.md, "plan-body", vec![]).scrollable(true))
                .into_any_element(),
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
            .when(n > 1, |d| d.child(crate::ui::hint("h l", "versions")))
            .when(!self.hunks.is_empty(), |d| {
                d.child(crate::ui::hint("] c", "next change"))
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
    use super::title;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin

    #[test]
    fn title_is_the_first_line_without_hashes() {
        assert_eq!(title("\n# Add a cache\n\nmore"), "Add a cache");
    }
}
