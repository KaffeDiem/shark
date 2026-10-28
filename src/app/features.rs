//! Workspace feature launchers. Each feature opens its content in an artifact tab.
use super::*;

impl Shark {
    pub(super) fn right_rail_shown(&self) -> bool {
        self.active.is_some() && !self.home_shown()
    }

    pub(super) fn render_features(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let (n, viewed) = (self.diff_files.len(), self.viewed_now.len());
        let tip: SharedString = match n {
            _ if self.diff_error.is_some() => {
                "Differ — Show diff (cmd-shift-d) · could not load".into()
            }
            _ if !self.diff_loaded => "Differ — Show diff (cmd-shift-d)".into(),
            0 => "Differ — Show diff (cmd-shift-d) · no changes".into(),
            n => format!("Differ — Show diff (cmd-shift-d) · {viewed}/{n} viewed").into(),
        };
        let badge = match () {
            _ if self.diff_error.is_some() => Some(theme.warning_foreground),
            _ if self.diff_loaded && viewed < n => Some(theme.primary),
            _ => None,
        };
        v_flex()
            .id("features-rail")
            .aria_label("Features")
            .key_context("Features")
            .track_focus(&self.features_focus)
            .on_action(cx.listener(|this, _: &Confirm, w, cx| {
                this.show(
                    if this.feature_sel == 0 {
                        Tab::Diff
                    } else {
                        Tab::Git
                    },
                    w,
                    cx,
                )
            }))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                this.feature_sel = 1;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| {
                this.feature_sel = 0;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| {
                this.feature_sel = 0;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| {
                this.feature_sel = 1;
                cx.notify();
            }))
            .w(px(72.))
            .h_full()
            .flex_none()
            .items_center()
            .child(
                div()
                    .id("features-header")
                    .h(px(44.))
                    .w_full()
                    .flex_none()
                    .map(|d| self.draggable_chrome("features-header", d)),
            )
            .child(
                v_flex().pt(px(if self.rail { 8. } else { 0. })).child(
                    sidebar::rail_button(
                        div().id("feature-differ").relative().flex().items_center(),
                    )
                    .role(Role::Button)
                    .aria_label("Differ — Show diff (cmd-shift-d)")
                    .text_color(theme.muted_foreground)
                    .map(|d| {
                        cursor_row(
                            d,
                            self.tab == Tab::Diff,
                            self.feature_sel == 0,
                            self.features_focus.is_focused(window),
                            theme,
                        )
                    })
                    .child(Icon::new(IconName::FileDiff).with_size(px(16.)))
                    .when_some(badge, |d, c| d.child(chats::dot(6., theme).bg(c)))
                    .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
                    .on_click(cx.listener(|this, _, w, cx| this.show(Tab::Diff, w, cx)))
                    .test_support(),
                ),
            )
            .child(
                sidebar::rail_button(div().id("feature-git").relative().flex().items_center())
                    .role(Role::Button)
                    .aria_label("Git — Pull requests (cmd-shift-g)")
                    .text_color(theme.muted_foreground)
                    .map(|d| {
                        cursor_row(
                            d,
                            self.tab == Tab::Git,
                            self.feature_sel == 1,
                            self.features_focus.is_focused(window),
                            theme,
                        )
                    })
                    .child(Icon::new(IconName::GitPullRequest).with_size(px(16.)))
                    .tooltip(|w, cx| Tooltip::new("Git — Pull requests (cmd-shift-g)").build(w, cx))
                    .on_click(cx.listener(|this, _, w, cx| this.show(Tab::Git, w, cx)))
                    .test_support(),
            )
            .test_support()
    }

    pub(super) fn active_git_feature(&self) -> Option<&Entity<git_feature::GitFeature>> {
        self.git_features.get(self.active.as_ref()?)
    }

    pub(super) fn ensure_git_feature(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<git_feature::GitFeature>> {
        let ws = self.active_ws()?.clone();
        let archiving = self.archiving.contains(&ws.name);
        if let Some(git) = self.git_features.get(&ws.name).cloned() {
            git.update(cx, |g, cx| {
                g.archiving = archiving;
                g.set_workspace(ws, window, cx);
            });
            return Some(git);
        }
        let name = ws.name.clone();
        let git = cx.new(|cx| {
            let mut git = git_feature::GitFeature::new(ws, window, cx);
            git.archiving = archiving;
            git
        });
        let workspace = name.clone();
        self._subs
            .push(cx.subscribe_in(&git, window, move |s, _, event, w, cx| {
                if s.active.as_ref() != Some(&workspace) {
                    return;
                }
                let git_feature::GitFeatureEvent::OpenFile { repo, path } = event;
                s.open_git_file(&format!("{repo}/{path}"), w, cx);
            }));
        self._subs.push(cx.observe(&git, |_, _, cx| cx.notify()));
        self.git_features.insert(name, git.clone());
        Some(git)
    }

    pub(super) fn open_git_file(
        &mut self,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self
            .diff_files
            .iter()
            .position(|f| f.path().as_ref() == path)
        else {
            return notify_err(
                window,
                cx,
                "This file is not in the current diff. Open the comment on GitHub to see its original context.",
            );
        };
        if self
            .editing
            .as_ref()
            .is_some_and(|e| e.path.as_ref() != path)
        {
            let (workspace, path) = (self.active.clone(), path.to_string());
            self.confirm_discard(false, true, window, cx, move |s, w, cx| {
                if s.active != workspace {
                    return;
                }
                s.end_edit(cx);
                s.open_git_file(&path, w, cx);
            });
            return;
        }
        self.show(Tab::Diff, window, cx);
        self.diff_file = ix;
        self.follow_tree(ix);
        self.review.at_file(path);
        self.diff.update(cx, |d, cx| {
            d.set_file_collapsed(path, false, cx);
            d.scroll_to_file(path, cx);
        });
        cx.notify();
    }

    pub(super) fn close_git(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.active_ws_mut() else {
            return;
        };
        ws.git_closed = true;
        ws.close_tab(&TabId::Git);
        let _ = self.state.save();
        if self.tab == Tab::Git {
            let ix = self.active_ws().unwrap().active_chat;
            self.select_chat(ix, window, cx);
        }
        cx.notify();
    }
}
