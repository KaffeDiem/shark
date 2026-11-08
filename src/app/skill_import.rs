//! Import portable skills into an agent's existing skills folder.
use super::*;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::text::TextViewState;
use std::sync::Arc;

pub(super) struct SkillImport {
    shark: WeakEntity<Shark>,
    slug: String,
    pub(super) source: Entity<InputState>,
    pub(super) prepared: Option<Arc<crate::skills::Import>>,
    pub(super) selected: Vec<bool>,
    preview: usize,
    instructions: Entity<TextViewState>,
    pub(super) busy: bool,
    installing: bool,
    pub(super) error: Option<String>,
    _source: Subscription,
}

impl Shark {
    pub(super) fn add_agent_skills(
        &mut self,
        slug: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<SkillImport>> {
        let agent = self.agent(slug)?;
        let title = format!("Add skills to {}", agent.name);
        let owner = cx.entity().downgrade();
        let panel = cx.new(|cx| SkillImport::new(owner, slug.to_string(), window, cx));
        let shown = panel.clone();
        window.open_dialog(cx, move |d, _, cx| {
            let installing = shown.read(cx).installing;
            let footer = shown.update(cx, |panel, cx| panel.footer(cx));
            crate::ui::dialog(d)
                .title(title.clone())
                .w(px(680.))
                .close_button(!installing)
                .overlay_closable(!installing)
                .keyboard(!installing)
                .on_ok(|_, _, _| false)
                .child(shown.clone())
                .footer(footer)
        });
        panel
            .read(cx)
            .source
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        Some(panel)
    }
}

impl SkillImport {
    fn new(
        shark: WeakEntity<Shark>,
        slug: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let source =
            cx.new(|cx| InputState::new(window, cx).placeholder("GitHub URL or local skill path"));
        let _source = cx.subscribe_in(&source, window, |this, _, event, window, cx| match event {
            InputEvent::Change if !this.busy => {
                this.prepared = None;
                this.selected.clear();
                this.error = None;
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.inspect(window, cx),
            _ => {}
        });
        Self {
            shark,
            slug,
            source,
            prepared: None,
            selected: vec![],
            preview: 0,
            instructions: cx.new(|cx| TextViewState::markdown("", cx)),
            busy: false,
            installing: false,
            error: None,
            _source,
        }
    }

    fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: false,
            prompt: Some("Choose skill folder or SKILL.md".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.first() else { return };
            this.update_in(cx, |this, window, cx| {
                this.source.update(cx, |input, cx| {
                    input.set_value(path.to_string_lossy().into_owned(), window, cx)
                });
                this.inspect(window, cx);
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn inspect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let source = self.source.read(cx).value().trim().to_string();
        if source.is_empty() {
            return;
        }
        self.busy = true;
        self.error = None;
        self.prepared = None;
        self.selected.clear();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { crate::skills::prepare(&source) })
                .await;
            this.update_in(cx, |this, _, cx| {
                this.busy = false;
                match result {
                    Ok(import) => {
                        this.selected = vec![import.skills.len() == 1; import.skills.len()];
                        this.prepared = Some(Arc::new(import));
                        this.show_preview(0, cx);
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn show_preview(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(skill) = self.prepared.as_ref().and_then(|p| p.skills.get(index)) else {
            return;
        };
        self.preview = index;
        self.instructions
            .update(cx, |text, cx| text.set_text(&skill.instructions, cx));
        cx.notify();
    }

    pub(super) fn install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(import) = self.prepared.clone() else {
            return;
        };
        if self.source.read(cx).value().trim() != import.source {
            self.prepared = None;
            self.selected.clear();
            cx.notify();
            return;
        }
        let selected: Vec<_> = self
            .selected
            .iter()
            .enumerate()
            .filter_map(|(i, selected)| selected.then_some(i))
            .collect();
        if selected.is_empty() {
            return;
        }
        let Some(shark) = self.shark.upgrade() else {
            return;
        };
        if shark.read(cx).agent(&self.slug).is_none() {
            self.error = Some("This agent no longer exists".into());
            cx.notify();
            return;
        }
        let slug = self.slug.clone();
        let count = selected.len();
        let example = import.skills[selected[0]].name.clone();
        let destination = crate::state::agent_skills_dir(&slug);
        self.busy = true;
        self.installing = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor()
                .spawn(async move { import.install(&destination, &selected) }).await;
            this.update_in(cx, |this, window, cx| {
                this.busy = false;
                this.installing = false;
                match result {
                    Err(error) => {
                        this.error = Some(error);
                        cx.notify();
                    }
                    Ok(()) => {
                        let refreshed = shark.update(cx, |shark, cx| shark.sync_agent_skills(&slug, cx));
                        window.close_dialog(cx);
                        match refreshed {
                            Ok(()) => window.push_notification(Notification::success(format!(
                                "Added {count} skill{}. Try ‘Use the {example} skill’ in chat. Start a new conversation if it is not discovered.",
                                if count == 1 { "" } else { "s" },
                            )), cx),
                            Err(error) => notify_err(window, cx, format!(
                                "Skills added, but some tasks could not refresh: {error}. Use Refresh skills to retry."
                            )),
                        }
                    }
                }
            }).ok();
        }).detach();
    }

    fn footer(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let count = self.selected.iter().filter(|selected| **selected).count();
        v_flex()
            .border_t_1()
            .border_color(theme.border)
            .pt_3()
            .gap_3()
            .when_some(self.error.clone(), |footer, error| {
                footer.child(
                    div()
                        .id("skill-import-error")
                        .test_support()
                        .max_h(px(80.))
                        .overflow_y_scroll()
                        .text_color(theme.danger_foreground)
                        .child(
                            div()
                                .id("skill-import-error-details")
                                .test_support()
                                .child(error),
                        ),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(if count == 0 {
                                "No skills selected".into()
                            } else {
                                format!("{count} selected")
                            }),
                    )
                    .child(
                        Button::new("skill-cancel")
                            .label("Cancel")
                            .disabled(self.installing)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("skill-add")
                            .primary()
                            .label(if count > 1 {
                                format!("Add {count} skills")
                            } else {
                                "Add skill".into()
                            })
                            .disabled(self.busy || count == 0)
                            .on_click(cx.listener(|this, _, window, cx| this.install(window, cx))),
                    ),
            )
    }
}

impl Render for SkillImport {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let source = v_flex()
            .flex_shrink_0()
            .gap_2()
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Source"))
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("Find skills from a GitHub URL or choose a local folder."),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .id("skill-source")
                            .test_support()
                            .flex_1()
                            .child(Input::new(&self.source).disabled(self.busy)),
                    )
                    .child(
                        Button::new("skill-inspect")
                            .label("Find skills")
                            .disabled(self.busy || self.source.read(cx).value().trim().is_empty())
                            .on_click(cx.listener(|this, _, window, cx| this.inspect(window, cx))),
                    ),
            )
            .child(
                Button::new("skill-browse")
                    .label("Choose local file or folder…")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| this.browse(window, cx))),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Choosing SKILL.md includes its folder and resources."),
            );

        // The dialog owns scrolling; sections and the preview keep their natural height.
        let mut content = v_flex()
            .id("skill-import")
            .test_support()
            .flex_shrink_0()
            .gap_4()
            .child(source);

        if self.busy {
            content = content.child(h_flex().gap_2().child(Spinner::new()).child(
                if self.installing {
                    "Adding skills…"
                } else {
                    "Finding skills…"
                },
            ));
        }
        if let Some(import) = &self.prepared {
            content = content.child(
                v_flex()
                    .id("skill-choices")
                    .test_support()
                    .flex_shrink_0()
                    .border_t_1()
                    .border_color(theme.border)
                    .pt_4()
                    .gap_2()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Skills to add"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("Select the skills you want to give this agent."),
                    )
                    .when(import.plugin, |section| {
                        section.child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("This source includes a plugin. Only its skill folders will be added; hooks and account connections require separate setup."),
                        )
                    })
                    .children(import.skills.iter().enumerate().map(|(index, skill)| {
                        h_flex()
                            .flex_shrink_0()
                            .gap_2()
                            .py_1()
                            .child(
                                Checkbox::new(("skill-choice", index))
                                    .label(skill.name.clone())
                                    .checked(self.selected[index])
                                    .on_click(cx.listener(move |this, checked, _, cx| {
                                        if !this.busy {
                                            this.selected[index] = *checked;
                                            this.show_preview(index, cx);
                                        }
                                    })),
                            )
                            .child(div().flex_1())
                            .child(
                                Button::new(("skill-preview", index))
                                    .ghost()
                                    .small()
                                    .label("Preview")
                                    .disabled(self.busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.show_preview(index, cx)
                                    })),
                            )
                    })),
            );
            if let Some(skill) = import.skills.get(self.preview) {
                content = content.child(
                    v_flex()
                        .id("skill-preview")
                        .test_support()
                        .flex_shrink_0()
                        .border_1()
                        .border_color(theme.border)
                        .rounded(theme.radius)
                        .bg(theme.secondary)
                        .p_3()
                        .gap_3()
                        .child(
                            div()
                                .id("skill-preview-heading")
                                .test_support()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("Preview"),
                        )
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(skill.name.clone()),
                        )
                        .child(div().child(skill.description.clone()))
                        .when_some(skill.compatibility.clone(), |d, requirements| {
                            d.child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("Requirements: {requirements}")),
                            )
                        })
                        .when(skill.has_scripts, |d| {
                            d.child(div().text_sm().text_color(theme.muted_foreground).child(
                                "Includes executable scripts. Adding the skill does not run them.",
                            ))
                        })
                        .child(
                            div()
                                .id("skill-instructions")
                                .test_support()
                                .border_t_1()
                                .border_color(theme.border)
                                .pt_3()
                                .text_sm()
                                .child(crate::ui::markdown(
                                    &self.instructions,
                                    "skill-instructions-text",
                                    vec![],
                                )),
                        ),
                );
            }
        }
        content
    }
}
