//! Workspace feature launchers. Each feature opens its content in an artifact tab.
use super::*;

#[derive(Clone, Copy)]
struct Metadata {
    title: &'static str,
    launcher_id: &'static str,
    launcher_label: &'static str,
    launcher_icon: IconName,
    tab_id: &'static str,
    tab_icon: IconName,
    tab_tip: &'static str,
    close_id: &'static str,
    close_label: &'static str,
}

impl Feature {
    pub(super) const ALL: [Self; 2] = [Self::Diff, Self::Git];

    pub(super) fn tab_id(self) -> TabId {
        match self {
            Self::Diff => TabId::Diff,
            Self::Git => TabId::Git,
        }
    }

    fn metadata(self) -> Metadata {
        match self {
            Self::Diff => Metadata {
                title: "Diff",
                launcher_id: "feature-differ",
                launcher_label: "Differ — Show diff (cmd-shift-d)",
                launcher_icon: IconName::FileDiff,
                tab_id: "diff-tab",
                tab_icon: IconName::GitCompare,
                tab_tip: "Diff (cmd-shift-d)",
                close_id: "close-diff",
                close_label: "Close Diff (cmd-w)",
            },
            Self::Git => Metadata {
                title: "Git",
                launcher_id: "feature-git",
                launcher_label: "Git — Pull requests (cmd-shift-g)",
                launcher_icon: IconName::GitPullRequest,
                tab_id: "git-tab",
                tab_icon: IconName::GitPullRequest,
                tab_tip: "Git (cmd-shift-g)",
                close_id: "close-git",
                close_label: "Close Git (cmd-w)",
            },
        }
    }
}

impl Shark {
    pub(super) fn right_rail_shown(&self) -> bool {
        self.active.is_some() && !self.home_shown() && !self.zen_shown()
    }

    pub(super) fn render_features(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .id("features-rail")
            .aria_label("Features")
            .key_context("Features")
            .track_focus(&self.features_focus)
            .on_action(cx.listener(|s, _: &Confirm, w, cx| {
                s.show(Tab::Feature(Feature::ALL[s.feature_sel]), w, cx)
            }))
            .on_action(cx.listener(|s, _: &SelectNext, _, cx| {
                s.feature_sel = (s.feature_sel + 1).min(Feature::ALL.len() - 1);
                cx.notify();
            }))
            .on_action(cx.listener(|s, _: &SelectPrev, _, cx| {
                s.feature_sel = s.feature_sel.saturating_sub(1);
                cx.notify();
            }))
            .on_action(cx.listener(|s, _: &SelectFirst, _, cx| {
                s.feature_sel = 0;
                cx.notify();
            }))
            .on_action(cx.listener(|s, _: &SelectLast, _, cx| {
                s.feature_sel = Feature::ALL.len() - 1;
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
            .children(Feature::ALL.into_iter().enumerate().map(|(ix, feature)| {
                let meta = feature.metadata();
                let (tip, badge) = match feature {
                    Feature::Diff => {
                        let status = self.differ.read(cx).status();
                        let (n, viewed) = (status.files, status.viewed);
                        let tip = match n {
                            _ if status.failed => {
                                format!("{} · could not load", meta.launcher_label)
                            }
                            _ if !status.loaded => meta.launcher_label.into(),
                            0 => format!("{} · no changes", meta.launcher_label),
                            n => format!("{} · {viewed}/{n} viewed", meta.launcher_label),
                        };
                        let badge = if status.failed {
                            Some(theme.warning_foreground)
                        } else if status.loaded && viewed < n {
                            Some(theme.primary)
                        } else {
                            None
                        };
                        (SharedString::from(tip), badge)
                    }
                    Feature::Git => (meta.launcher_label.into(), None),
                };
                v_flex().when(ix == 0 && self.rail, |d| d.pt(px(8.))).child(
                    sidebar::rail_button(
                        div().id(meta.launcher_id).relative().flex().items_center(),
                    )
                    .role(Role::Button)
                    .aria_label(meta.launcher_label)
                    .text_color(theme.muted_foreground)
                    .map(|d| {
                        cursor_row(
                            d,
                            self.tab == Tab::Feature(feature),
                            self.feature_sel == ix,
                            self.features_focus.is_focused(window),
                            theme,
                        )
                    })
                    .child(Icon::new(meta.launcher_icon).with_size(px(16.)))
                    .when_some(badge, |d, c| d.child(chats::dot(6., theme).bg(c)))
                    .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
                    .on_click(cx.listener(move |s, _, w, cx| s.show(Tab::Feature(feature), w, cx)))
                    .test_support(),
                )
            }))
            .test_support()
    }

    pub(super) fn render_feature_tab(
        &self,
        feature: Feature,
        ix: usize,
        ctrl_held: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let meta = feature.metadata();
        let theme = cx.theme();
        let on = self.tab == Tab::Feature(feature);
        pill(meta.tab_id, on, theme)
            .map(|d| self.tab_drag(d, feature.tab_id(), meta.title.into(), cx))
            .aria_label(meta.title)
            .pl_2p5()
            .pr_1()
            .on_click(cx.listener(move |s, _, w, cx| s.show(Tab::Feature(feature), w, cx)))
            .child(if ctrl_held && ix < 9 {
                kbd(&format!("ctrl-{}", ix + 1)).into_any_element()
            } else {
                Icon::new(meta.tab_icon)
                    .with_size(px(14.))
                    .into_any_element()
            })
            .child(
                tab_label(meta.title.into(), on, theme)
                    .id("tab-label")
                    .test_support(),
            )
            .child(tab_close(
                crate::settings::close_button(meta.close_id, meta.close_label).on_click(
                    cx.listener(move |s, _, w, cx| {
                        cx.stop_propagation();
                        s.close_feature(feature, w, cx);
                    }),
                ),
                theme,
            ))
            .tooltip(move |w, cx| Tooltip::new(meta.tab_tip).build(w, cx))
            .test_support()
            .into_any_element()
    }

    pub(super) fn close_feature(
        &mut self,
        feature: Feature,
        w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match feature {
            Feature::Diff => self.close_diff(w, cx),
            Feature::Git => self.close_git(w, cx),
        }
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
                g.set_archiving(archiving, cx);
                g.set_workspace(ws, window, cx);
            });
            return Some(git);
        }
        let name = ws.name.clone();
        let git = cx.new(|cx| {
            let mut git = git_feature::GitFeature::new(ws, window, cx);
            git.set_archiving(archiving, cx);
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
        self.configure_differ(cx);
        if self
            .differ
            .update(cx, |d, cx| d.open_path(path, window, cx))
        {
            self.show(Tab::Feature(Feature::Diff), window, cx);
        }
    }

    pub(super) fn close_git(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let at = self.active_tab_index(cx);
        let Some(ws) = self.active_ws_mut() else {
            return;
        };
        ws.git_closed = true;
        ws.close_tab(&TabId::Git);
        let _ = self.state.save();
        if self.tab == Tab::Feature(Feature::Git) && !self.home_shown() {
            self.select_neighbor(at.unwrap_or(0), window, cx);
        }
        cx.notify();
    }
}

impl Shark {
    pub(super) fn activate_feature(
        &mut self,
        feature: Feature,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match feature {
            Feature::Diff => self.differ.update(cx, |d, cx| d.activate(window, cx)),
            Feature::Git => {
                if let Some(git) = self.ensure_git_feature(window, cx) {
                    git.update(cx, |g, cx| g.focus(window, cx));
                }
            }
        }
    }

    pub(super) fn feature_focus_targets(&self, feature: Feature, cx: &App) -> Vec<FeatureFocus> {
        match feature {
            Feature::Diff => self.differ.read(cx).focus_targets(),
            Feature::Git => vec![FeatureFocus::Main],
        }
    }

    pub(super) fn focused_feature(
        &self,
        feature: Feature,
        window: &Window,
        cx: &App,
    ) -> Option<FeatureFocus> {
        match feature {
            Feature::Diff => self.differ.read(cx).focused(window, cx),
            Feature::Git => self
                .active_git_feature()
                .filter(|g| g.read(cx).has_focus(window, cx))
                .map(|_| FeatureFocus::Main),
        }
    }

    pub(super) fn focus_feature(
        &mut self,
        feature: Feature,
        target: FeatureFocus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match feature {
            Feature::Diff => self.differ.update(cx, |d, cx| d.focus(target, window, cx)),
            Feature::Git => {
                if let Some(git) = self.active_git_feature().cloned() {
                    git.update(cx, |g, cx| g.focus(window, cx));
                }
            }
        }
    }

    pub(super) fn render_feature_controls(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        match self.tab {
            Tab::Feature(Feature::Diff) => Some(
                self.differ
                    .update(cx, |d, cx| d.render_diff_controls(cx).into_any_element()),
            ),
            _ => None,
        }
    }

    pub(super) fn render_feature(&self, feature: Feature, _cx: &App) -> AnyElement {
        let content = match feature {
            Feature::Diff => self.differ.clone().into_any_element(),
            Feature::Git => self
                .active_git_feature()
                .map(|g| {
                    g.clone()
                        .cached(StyleRefinement::default().size_full())
                        .into_any_element()
                })
                .unwrap_or_else(|| div().into_any_element()),
        };
        div()
            .flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(content)
            .into_any_element()
    }

    /// Global Save follows the active artifact, including when focus is in the rail.
    pub(super) fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.tab {
            Tab::Feature(Feature::Git) => {
                if self.home_shown() {
                    return;
                }
                if let Some(git) = self.active_git_feature().cloned() {
                    git.update(cx, |g, cx| g.submit(window, cx));
                }
            }
            Tab::Feature(Feature::Diff) if self.differ.read(cx).edit_snapshot().is_some() => {
                self.differ
                    .update(cx, |d, cx| d.save_edit(false, window, cx));
            }
            _ => {
                if let Some(editor) = &self.editor {
                    editor.update(cx, |e, cx| e.save(false, window, cx));
                }
            }
        }
    }
}
