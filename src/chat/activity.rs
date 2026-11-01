//! Background tasks borrow their portraits from the user's caught Pokémon.
use super::*;
use std::sync::Arc;

pub(crate) type Appearance = (u16, bool, Option<crate::pokemon::forms::Form>);
pub(super) const PARADE_LIMIT: usize = 6;
pub(super) const HOP_INTERVAL: std::time::Duration = std::time::Duration::from_millis(400);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pokemon {
    pub id: String,
    pub name: String,
    pub appearance: Appearance,
}

pub(crate) fn roster(companion: &crate::pokemon::Companion) -> Vec<Pokemon> {
    companion
        .collection
        .iter()
        .chain(companion.active.iter())
        .enumerate()
        .map(|(i, mon)| {
            let species = mon.species_id();
            Pokemon {
                // Saves are sanitized before publication; the fallback also supports old fixtures.
                id: mon
                    .profile
                    .as_ref()
                    .map(|p| p.instance_id.clone())
                    .filter(|id| !id.is_empty())
                    .unwrap_or_else(|| format!("unprofiled-{i}")),
                name: if mon.displays_shiny() {
                    format!("Shiny {}", mon.name())
                } else {
                    mon.name()
                },
                appearance: (
                    species,
                    mon.displays_shiny(),
                    crate::pokemon::forms::resolved(species, mon.form.as_ref()),
                ),
            }
        })
        .collect()
}

pub(super) fn assign_pokemon(
    assignments: &mut HashMap<String, String>,
    tasks: &[agent::Task],
    roster: &[Pokemon],
) {
    assignments.retain(|task, individual| {
        tasks
            .iter()
            .any(|t| &t.id == task && t.kind != "local_bash")
            && roster.iter().any(|p| &p.id == individual)
    });
    for task in tasks.iter().filter(|t| t.kind != "local_bash") {
        if !assignments.contains_key(&task.id) {
            if let Some(pokemon) = roster
                .iter()
                .min_by_key(|p| assignments.values().filter(|id| *id == &p.id).count())
            {
                assignments.insert(task.id.clone(), pokemon.id.clone());
            }
        }
    }
}

pub(super) fn assigned<'a>(
    task: &agent::Task,
    assignments: &HashMap<String, String>,
    roster: &'a [Pokemon],
) -> Option<&'a Pokemon> {
    let id = assignments.get(&task.id)?;
    roster.iter().find(|p| &p.id == id)
}

fn portrait_element(task: &agent::Task, image: Option<&Arc<RenderImage>>) -> AnyElement {
    let content = match image {
        // Use the shared, cached first frame. A single 400ms hop timer animates the whole
        // row; six GIFs must not each request continuous display-rate redraws.
        Some(image) => crate::ui::pokemon_images::paint_frame(image.clone(), 0, 20.),
        None => Icon::new(if task.kind == "local_bash" {
            IconName::SquareTerminal
        } else {
            IconName::Bot
        })
        .with_size(px(12.))
        .into_any_element(),
    };
    div()
        .size(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .child(content)
        .into_any_element()
}

impl Chat {
    pub(crate) fn sync_activity_roster(
        &mut self,
        roster: &[Pokemon],
        cx: &mut Context<Self>,
    ) -> Vec<Appearance> {
        let changed = self.activity_roster != roster;
        if changed {
            self.activity_roster = roster.to_vec();
            assign_pokemon(&mut self.task_pokemon, &self.background, roster);
        }
        let wanted: Vec<_> = self
            .background
            .iter()
            .filter_map(|t| assigned(t, &self.task_pokemon, roster).map(|p| p.appearance.clone()))
            .collect();
        self.activity_portraits
            .retain(|key, _| wanted.contains(key));
        self.refresh_activity_animation(cx);
        if changed {
            cx.notify();
        }
        wanted
            .into_iter()
            .filter(|key| !self.activity_portraits.contains_key(key))
            .collect()
    }

    pub(crate) fn set_activity_portrait(
        &mut self,
        appearance: Appearance,
        image: Arc<RenderImage>,
        cx: &mut Context<Self>,
    ) {
        self.activity_portraits.insert(appearance, image);
        self.refresh_activity_animation(cx);
        cx.notify();
    }

    pub(super) fn set_background(&mut self, tasks: Vec<agent::Task>, cx: &mut Context<Self>) {
        assign_pokemon(&mut self.task_pokemon, &tasks, &self.activity_roster);
        self.background = tasks;
        self.refresh_activity_animation(cx);
    }

    fn refresh_activity_animation(&mut self, cx: &mut Context<Self>) {
        let animate = self.background.iter().take(PARADE_LIMIT).any(|task| {
            assigned(task, &self.task_pokemon, &self.activity_roster)
                .is_some_and(|p| self.activity_portraits.contains_key(&p.appearance))
        });
        if !animate || cx.reduce_motion() || self.replaying {
            self.hopping = None;
            self.hop = 0;
        } else if self.hopping.is_none() {
            self.hopping = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(HOP_INTERVAL).await;
                    let keep_hopping = this.update(cx, |c, cx| {
                        if c.background.is_empty() || cx.reduce_motion() {
                            c.hopping = None;
                            return false;
                        }
                        c.hop = c.hop.wrapping_add(1);
                        cx.notify();
                        true
                    });
                    if !matches!(keep_hopping, Ok(true)) {
                        break;
                    }
                }
            }));
        }
    }

    /// The list's last item, under the newest message: a quiet line while a turn or background
    /// tasks run; hovering lists the details. Fixed height, so it comes and goes
    /// without remeasuring or moving the transcript.
    pub(super) fn render_activity(&self, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let line = div()
            .id("activity")
            .test_support()
            .w_full()
            .h(px(24.))
            .px_4();
        if !self.working() {
            return line;
        }
        let text = match (self.busy, self.background.len()) {
            _ if self.asking() => "Waiting for your answers".into(),
            (true, 0) => "Working".into(),
            (true, n) => format!("Working · {n} in background"),
            (false, n) => format!("{n} in background"),
        };
        let (busy, started, model) = (
            self.busy && !self.asking(),
            self.started,
            self.model().to_string(),
        );
        let (queued, steered, tasks) = (self.queued(), self.steered, self.background.clone());
        let avatars: Vec<_> = tasks
            .iter()
            .map(|t| assigned(t, &self.task_pokemon, &self.activity_roster).cloned())
            .collect();
        let portraits = self.activity_portraits.clone();
        let visible = tasks.len().min(PARADE_LIMIT);
        line.child(
            div().w_full().max_w(px(680.)).mx_auto().child(
                HoverCard::new("activity-card")
                    .anchor(Anchor::BottomLeft)
                    .trigger(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap_1()
                            .h(px(24.))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .children(tasks.iter().take(PARADE_LIMIT).enumerate().map(|(i, t)| {
                                let pokemon = avatars[i].as_ref();
                                let portrait = pokemon.and_then(|p| portraits.get(&p.appearance));
                                // Fixed slots contain the hop without moving the transcript.
                                div()
                                    .id(("background-pokemon", i))
                                    .test_support()
                                    .role(Role::Image)
                                    .aria_label(match pokemon {
                                        Some(p) => format!("{} · {}", p.name, t.description),
                                        None => t.description.clone(),
                                    })
                                    .size(px(20.))
                                    .flex_shrink_0()
                                    .child(
                                        div()
                                            .when(
                                                portrait.is_some()
                                                    && !cx.reduce_motion()
                                                    && i == self.hop % visible,
                                                |d| d.mt(px(-2.)),
                                            )
                                            .child(portrait_element(t, portrait)),
                                    )
                            }))
                            .when(tasks.len() > PARADE_LIMIT, |d| {
                                d.child(format!("+{}", tasks.len() - PARADE_LIMIT))
                            })
                            // The shimmer redraws the chat every frame, as a tool's spinner
                            // does; only during a turn, not for tasks left running after it.
                            .map(|d| match busy {
                                true => d.child(Spinner::new().with_size(px(12.))).child(
                                    div()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .child(ShimmerText::new(text).id("activity-text")),
                                ),
                                false => d.child(
                                    div()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .child(text),
                                ),
                            }),
                    )
                    .content(move |_, _, cx| {
                        let theme = cx.theme();
                        let line = |t: String| div().child(t);
                        v_flex()
                            .id("activity-details")
                            .test_support()
                            .max_h(px(320.))
                            .overflow_y_scroll()
                            .gap_1()
                            .text_xs()
                            .max_w(px(420.))
                            .when_some(started.filter(|_| busy), |d, t| {
                                d.child(line(format!(
                                    "Turn running for {}",
                                    duration(t.elapsed().as_secs())
                                )))
                            })
                            .child(line(format!("Model {model}")))
                            .when(queued > 0, |d| d.child(line(format!("{queued} queued"))))
                            .when(steered, |d| {
                                d.child(line("Message sent mid-turn, not read yet".into()))
                            })
                            .when(!tasks.is_empty(), |d| {
                                d.child(
                                    div()
                                        .pt_1()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("{} in background", tasks.len())),
                                )
                                .children(
                                    tasks.iter().enumerate().map(|(i, t)| {
                                        let pokemon = avatars[i].as_ref();
                                        h_flex()
                                            .id(("activity-task", i))
                                            .test_support()
                                            .gap_1p5()
                                            .child(portrait_element(
                                                t,
                                                pokemon.and_then(|p| portraits.get(&p.appearance)),
                                            ))
                                            .child(match pokemon {
                                                Some(p) => {
                                                    format!("{} · {}", p.name, t.description)
                                                }
                                                None => t.description.clone(),
                                            })
                                    }),
                                )
                            })
                    }),
            ),
        )
    }
}
