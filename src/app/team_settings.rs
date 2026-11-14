//! The New team dialog and a team's menu: give a goal, pause, rename, delete.
use super::*;
use gpui_kit::component::Selectable as _;

/// The New team dialog's fields.
struct NewTeamForm {
    shark: WeakEntity<Shark>,
    name: Entity<InputState>,
    /// Every agent: slug and name.
    agents: Vec<(String, String)>,
    repos: Vec<String>,
    lead: String,
    members: Vec<String>,
    picked: Vec<String>,
    error: Option<String>,
}

fn toggle(list: &mut Vec<String>, item: &str) {
    match list.iter().position(|i| i == item) {
        Some(at) => drop(list.remove(at)),
        None => list.push(item.into()),
    }
}

impl NewTeamForm {
    /// Creates the team and shows its page. A refused team keeps the dialog and says why.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let name = self.name.read(cx).value().to_string();
        let (lead, members, repos) = (self.lead.clone(), self.members.clone(), self.picked.clone());
        let made = (self.shark)
            .update(cx, |s, cx| {
                let slug = s.create_team(&name, &lead, members, repos)?;
                // Closing first leaves the focus to the page.
                window.close_dialog(cx);
                s.open_team(&slug, window, cx);
                Ok::<_, String>(())
            })
            .unwrap_or(Ok(()));
        self.error = made.err();
        cx.notify();
        false
    }

    /// Cancel and Create team.
    fn footer(&mut self, cx: &mut Context<Self>) -> Div {
        h_flex()
            .w_full()
            .gap_2()
            .child(div().flex_1())
            .child(
                Button::new("team-cancel")
                    .label("Cancel")
                    .on_click(|_, w, cx| w.close_dialog(cx)),
            )
            .child(
                Button::new("team-create")
                    .primary()
                    .label("Create team")
                    .on_click(cx.listener(|f, _, w, cx| {
                        f.submit(w, cx);
                    })),
            )
    }

    /// A row of toggle pills.
    fn pills(
        &self,
        id: &'static str,
        items: impl Iterator<Item = (String, String, bool)>,
        pick: fn(&mut Self, &str),
        cx: &mut Context<Self>,
    ) -> Div {
        h_flex()
            .gap_1()
            .flex_wrap()
            .children(items.enumerate().map(|(i, (slug, label, on))| {
                Button::new((id, i))
                    .ghost()
                    .small()
                    .label(label)
                    .selected(on)
                    .toggled(on)
                    .on_click(cx.listener(move |f, _, _, cx| {
                        pick(f, &slug);
                        cx.notify();
                    }))
            }))
    }
}

impl Render for NewTeamForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let lead = self.pills(
            "new-team-lead",
            self.agents
                .iter()
                .map(|(s, n)| (s.clone(), n.clone(), *s == self.lead)),
            |f, slug| {
                f.lead = slug.into();
                f.members.retain(|m| m != slug);
            },
            cx,
        );
        let agents = self.pills(
            "new-team-agent",
            (self.agents.iter())
                .filter(|(s, _)| *s != self.lead)
                .map(|(s, n)| (s.clone(), n.clone(), self.members.contains(s))),
            |f, slug| toggle(&mut f.members, slug),
            cx,
        );
        let repos = self.pills(
            "new-team-repo",
            self.repos
                .iter()
                .map(|r| (r.clone(), r.clone(), self.picked.contains(r))),
            |f, slug| toggle(&mut f.picked, slug),
            cx,
        );
        let theme = cx.theme();
        let label = |text: &'static str| {
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.muted_foreground)
                .child(text)
        };
        v_flex()
            .gap_3()
            .child(
                v_flex()
                    .gap_1()
                    .child(label("Name"))
                    .child(Input::new(&self.name)),
            )
            .child(v_flex().gap_1().child(label("Lead")).child(lead))
            .child(v_flex().gap_1().child(label("Agents")).child(agents))
            .child(v_flex().gap_1().child(label("Repositories")).child(repos))
            .children(self.error.clone().map(|e| {
                div()
                    .id("new-team-error")
                    .test_support()
                    .text_sm()
                    .text_color(theme.danger_foreground)
                    .child(e)
            }))
    }
}

impl Shark {
    /// **New team…**: one dialog with a name, the lead, the agents and the repositories.
    pub(super) fn new_team_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let agents: Vec<_> = (self.state.agents.iter())
            .map(|a| (a.slug.clone(), a.name.clone()))
            .collect();
        if agents.len() < 2 {
            return notify_err(
                window,
                cx,
                "A team needs two agents. Create one with New agent…",
            );
        }
        let lead = agents[0].0.clone();
        let repos: Vec<String> = self.state.repos.iter().map(|r| r.name.clone()).collect();
        if repos.is_empty() {
            return notify_err(window, cx, "Add a repo first (Add repo in cmd-k)");
        }
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Team name"));
        let shark = cx.entity().downgrade();
        let form = cx.new(|_| NewTeamForm {
            shark,
            name: name.clone(),
            members: agents.iter().skip(1).map(|a| a.0.clone()).collect(),
            picked: repos.iter().take(1).cloned().collect(),
            agents,
            repos,
            lead,
            error: None,
        });
        window.open_dialog(cx, move |d, _, cx| {
            let f = form.clone();
            crate::ui::dialog(d)
                .title("New team")
                .w(px(560.))
                .child(form.clone())
                .footer(form.update(cx, |f, cx| f.footer(cx)))
                .on_ok(move |_, window, cx| f.update(cx, |f, cx| f.submit(window, cx)))
        });
        name.update(cx, |i, cx| i.focus(window, cx));
    }

    /// A name for `slug`, asked in a dialog.
    fn rename_team(&mut self, slug: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(team) = self.team(slug) else { return };
        let current = team.name.clone();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Team name")
                .default_value(current)
        });
        let (field, shark, slug) = (input.clone(), cx.entity().downgrade(), slug.to_string());
        window.open_dialog(cx, move |d, _, _| {
            let (input, shark, slug) = (input.clone(), shark.clone(), slug.clone());
            crate::ui::dialog(d)
                .title("Rename team")
                .w(px(440.))
                .child(Input::new(&input))
                .on_ok(move |_, window, cx| {
                    let name = input.read(cx).value().trim().to_string();
                    if name.is_empty() {
                        return false;
                    }
                    shark
                        .update(cx, |s, cx| {
                            s.update_team(&slug, window, cx, |t| t.name = name)
                        })
                        .ok();
                    true
                })
        });
        field.update(cx, |i, cx| {
            i.focus(window, cx);
            i.select_all(window, cx);
        });
    }

    /// A team's actions, for its sidebar row's context menu and its page.
    pub(super) fn team_menu(&self, slug: &str, cx: &Context<Self>) -> Vec<Entry> {
        let Some(team) = self.team(slug) else {
            return vec![];
        };
        let paused = team.paused;
        let (a, b, c, d) = (
            slug.to_string(),
            slug.to_string(),
            slug.to_string(),
            slug.to_string(),
        );
        vec![
            menu::item(
                IconName::Plus,
                "Give a goal…",
                on_shark(cx, move |s, w, cx| s.open_team_composer(&a, w, cx)),
            ),
            menu::item(
                if paused {
                    IconName::Play
                } else {
                    IconName::Pause
                },
                if paused { "Resume team" } else { "Pause team" },
                on_shark(cx, move |s, w, cx| s.toggle_team_paused(&b, w, cx)),
            ),
            menu::item(
                IconName::Pencil,
                "Rename…",
                on_shark(cx, move |s, w, cx| s.rename_team(&c, w, cx)),
            ),
            Entry::Sep,
            menu::item(
                IconName::Trash,
                "Delete team…",
                on_shark(cx, move |s, w, cx| s.confirm_delete_team(&d, w, cx)),
            ),
        ]
    }

    pub(super) fn confirm_delete_team(
        &mut self,
        slug: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(team) = self.team(slug) else { return };
        // `delete_team` refuses while a goal is open; say so before asking.
        if self.team_goals(slug).any(|(_, g)| g.state.open()) {
            return notify_err(window, cx, "Stop or finish the team's goals first");
        }
        let slug = slug.to_string();
        crate::ui::confirm(
            format!("Delete team {}?", team.name),
            "Its goals and their tasks stay, as ordinary tasks.",
            "Delete team",
            move |s: &mut Shark, w, cx| {
                if let Err(e) = s.delete_team(&slug, cx) {
                    notify_err(w, cx, e);
                }
            },
            window,
            cx,
        );
    }
}

impl Shark {
    /// A picker of the teams; `then` gets the one chosen.
    fn pick_team(
        &mut self,
        placeholder: &'static str,
        then: fn(&mut Shark, &str, &mut Window, &mut Context<Shark>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let teams: Vec<(String, String)> = (self.state.teams.iter())
            .map(|t| (t.slug.clone(), t.name.clone()))
            .collect();
        if teams.is_empty() {
            return self.new_team_dialog(window, cx);
        }
        let items = (teams.iter())
            .map(|(_, name)| CommandItem::new().label(name.clone()).icon(IconName::Users))
            .collect();
        open_menu(
            cx.entity().downgrade(),
            placeholder,
            items,
            0,
            move |s, i, w, cx| then(s, &teams[i].0, w, cx),
            window,
            cx,
        );
    }

    /// **Give a goal to…**: pick a team, then describe the goal on its page.
    pub(super) fn give_goal_to(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pick_team(
            "Give a goal to…",
            |s, slug, w, cx| s.open_team_composer(slug, w, cx),
            window,
            cx,
        );
    }

    /// **Open team…**.
    pub(super) fn open_team_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pick_team(
            "Open team…",
            |s, slug, w, cx| s.open_team(slug, w, cx),
            window,
            cx,
        );
    }

    /// Flips a team between paused and running.
    pub(super) fn toggle_team_paused(
        &mut self,
        slug: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paused = self.team(slug).is_some_and(|t| t.paused);
        self.update_team(slug, window, cx, |t| t.paused = !paused)
    }

    /// Runs `then` on the team on screen, else on one picked.
    fn on_shown_team(
        &mut self,
        placeholder: &'static str,
        then: fn(&mut Shark, &str, &mut Window, &mut Context<Shark>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.team_shown().map(str::to_string) {
            Some(slug) => then(self, &slug, window, cx),
            None => self.pick_team(placeholder, then, window, cx),
        }
    }

    /// **Pause team**: the team on screen, else a picker. Resumes a paused one.
    pub(super) fn pause_team(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.on_shown_team("Pause team…", Shark::toggle_team_paused, window, cx)
    }

    /// **Rename team…**.
    pub(super) fn rename_shown_team(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.on_shown_team("Rename team…", Shark::rename_team, window, cx)
    }

    /// **Delete team…**.
    pub(super) fn delete_shown_team(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.on_shown_team("Delete team…", Shark::confirm_delete_team, window, cx)
    }

    /// Whether the team on screen is paused, for the labels that read Resume then.
    pub(crate) fn shown_team_paused(&self) -> bool {
        (self.team_shown().and_then(|s| self.team(s))).is_some_and(|t| t.paused)
    }
}
