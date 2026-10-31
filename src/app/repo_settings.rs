//! Repo settings (click a repo in the sidebar): new workspace, default model, setup, run and
//! archive scripts, removal. Every change saves at once, as in Settings.
use super::*;
use crate::chat::{effort_label, model_label};
use crate::models::efforts;
use crate::scripts::Phase;
use crate::state::RepoSettings;
use crate::ui::{card, row, section};
use gpui_kit::component::IndexPath;
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};

/// Title, description and placeholder.
fn info(p: Phase) -> (&'static str, &'static str, &'static str) {
    match p {
        Phase::Setup => (
            "Setup",
            "Run in each new worktree of this repo, in order",
            "npm install  or  cp $SHARK_ROOT_PATH/.env .",
        ),
        Phase::Run => (
            "Run",
            "cmd-r starts these and stops them; across repos, drag the repos in the sidebar. $SHARK_PORT is a free block of 10 ports",
            "npm run dev -- --port $SHARK_PORT",
        ),
        Phase::Archive => (
            "Archive",
            "Run before the worktree is removed",
            "docker compose down",
        ),
    }
}

type Choice = Entity<SelectState<SearchableVec<SharedString>>>;

/// A script row being dragged by its grip: which list and where from.
#[derive(Clone)]
struct ScriptDrag {
    phase: Phase,
    ix: usize,
    label: SharedString,
}

impl Render for ScriptDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_2()
            .py_1()
            .rounded(theme.radius)
            .bg(theme.secondary)
            .border_1()
            .border_color(theme.border)
            .font_family(theme.mono_font_family.clone())
            .text_size(theme.mono_font_size)
            .child(self.label.clone())
    }
}

pub(super) struct RepoSettingsView {
    shark: WeakEntity<Shark>,
    repo: Repo,
    /// What "Last used" means now, so its levels (Claude's or Codex's) are the ones offered.
    last: String,
    model: Choice,
    effort: Choice,
    /// One input per script, per `Phase` (indexed `as usize`); saved on every change.
    scripts: [Vec<Entity<InputState>>; 3],
    _subs: Vec<Subscription>,
}

/// The levels `s`'s model takes; "Last used" is `last`.
fn levels(s: &RepoSettings, last: &str) -> &'static [&'static str] {
    efforts(s.model.as_deref().unwrap_or(last))
}

pub(super) fn open(shark: &mut Shark, repo: Repo, window: &mut Window, cx: &mut Context<Shark>) {
    let settings = shark.state.repo_settings.get(&repo.name).cloned();
    let weak = cx.entity().downgrade();
    let last = shark.state.new_model();
    let view = cx.new(|cx| {
        RepoSettingsView::new(weak, repo, last, settings.unwrap_or_default(), window, cx)
    });
    window.open_dialog(cx, move |d, _, _| {
        crate::ui::dialog(d).w(px(640.)).child(view.clone())
    });
}

impl RepoSettingsView {
    fn new(
        shark: WeakEntity<Shark>,
        repo: Repo,
        last: String,
        s: RepoSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut models = vec![SharedString::from("Last used")];
        models.extend(MODELS.iter().map(|m| model_label(m).into()));
        let at = (s.model.as_deref())
            .and_then(|m| MODELS.iter().position(|x| *x == m))
            .map_or(0, |i| i + 1);
        let model = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(models),
                Some(IndexPath::new(at)),
                window,
                cx,
            )
        });
        let effort = Self::effort_choice(&s, &last, window, cx);
        let mut subs = vec![cx.subscribe_in(&model, window, |this, _, ev, window, cx| {
            let SelectEvent::Confirm(Some(v)) = ev else {
                return;
            };
            let m = MODELS.iter().find(|m| model_label(m) == v.as_ref());
            let last = this.last.clone();
            let s = this.update(cx, |s| {
                s.model = m.map(|m| m.to_string());
                let levels = levels(s, &last);
                s.effort = s.effort.take().filter(|e| levels.contains(&e.as_str()));
            });
            // Claude and Codex take different levels.
            this.effort = Self::effort_choice(&s, &this.last, window, cx);
            this._subs.push(this.watch_effort(window, cx));
        })];
        let mut this = RepoSettingsView {
            shark,
            repo,
            last,
            model,
            effort,
            scripts: Default::default(),
            _subs: vec![],
        };
        subs.push(this.watch_effort(window, cx));
        for p in Phase::ALL {
            for script in s.phase(p) {
                this.add_script(p, script, window, cx);
            }
        }
        this._subs.extend(subs);
        this
    }

    fn effort_choice(
        s: &RepoSettings,
        last: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Choice {
        let levels = levels(s, last);
        let mut items = vec![SharedString::from("Last used")];
        items.extend(levels.iter().map(|l| effort_label(l).into()));
        let at = (s.effort.as_deref())
            .and_then(|e| levels.iter().position(|l| *l == e))
            .map_or(0, |i| i + 1);
        cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(items),
                Some(IndexPath::new(at)),
                window,
                cx,
            )
        })
    }

    fn watch_effort(&self, window: &mut Window, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe_in(&self.effort, window, |this, _, ev, _, cx| {
            let SelectEvent::Confirm(Some(v)) = ev else {
                return;
            };
            let v = v.to_string();
            let last = this.last.clone();
            this.update(cx, move |s| {
                let levels = levels(s, &last);
                s.effort = (levels.iter().find(|l| effort_label(l) == v)).map(|l| l.to_string());
            });
        })
    }

    /// Edits this repo's saved settings; an all-default entry is dropped.
    fn update(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut RepoSettings)) -> RepoSettings {
        let name = self.repo.name.clone();
        let s = self.shark.update(cx, |sh, cx| {
            let map = &mut sh.state.repo_settings;
            let mut s = map.remove(&name).unwrap_or_default();
            f(&mut s);
            if s != RepoSettings::default() {
                map.insert(name, s.clone());
            }
            let _ = sh.state.save();
            cx.notify();
            s
        });
        cx.notify();
        s.unwrap_or_default()
    }

    /// Saves the script inputs as they read now; blank ones are skipped.
    fn save_scripts(&self, cx: &mut Context<Self>) {
        let lists: Vec<Vec<String>> = (self.scripts.iter())
            .map(|l| {
                (l.iter().map(|i| i.read(cx).value().trim().to_string()))
                    .filter(|v| !v.is_empty())
                    .collect()
            })
            .collect();
        self.update(cx, |s| {
            for p in Phase::ALL {
                *s.phase_mut(p) = lists[p as usize].clone();
            }
        });
    }

    fn add_script(
        &mut self,
        phase: Phase,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let hint = info(phase).2;
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(hint)
                .default_value(text.to_string())
        });
        self._subs
            .push(cx.subscribe(&input, |this, _, ev: &InputEvent, cx| {
                if let InputEvent::Change = ev {
                    this.save_scripts(cx);
                }
            }));
        self.scripts[phase as usize].push(input);
    }

    fn remove_script(&mut self, phase: Phase, ix: usize, cx: &mut Context<Self>) {
        self.scripts[phase as usize].remove(ix);
        self.save_scripts(cx);
    }

    fn move_script(&mut self, d: &ScriptDrag, to: usize, cx: &mut Context<Self>) {
        let list = &mut self.scripts[d.phase as usize];
        if d.ix != to && d.ix < list.len() && to < list.len() {
            move_item(list, d.ix, to);
            self.save_scripts(cx);
        }
    }

    fn new_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.repo.name.clone();
        window.close_dialog(cx);
        (self.shark)
            .update(cx, |s, cx| s.new_workspace(vec![name], None, window, cx))
            .ok();
    }

    fn remove_repo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.repo.name.clone();
        let used: Vec<String> = (self.shark.upgrade())
            .map(|s| {
                let s = s.read(cx);
                // A workspace being created or getting a repo isn't in `workspaces` yet.
                (s.state.workspaces.iter())
                    .filter(|w| w.repos.iter().any(|r| r.name == name))
                    .map(|w| w.name.clone())
                    .chain(s.creating.iter().chain(&s.adding_repos).cloned())
                    .collect()
            })
            .unwrap_or_default();
        if !used.is_empty() {
            let msg = format!("{name} is in {}; archive those first", used.join(", "));
            return notify_err(window, cx, msg);
        }
        confirm(
            format!("Remove {name} from Shark?"),
            "Its folder stays where it is; its scripts and defaults here are forgotten.",
            "Remove",
            move |this: &mut Self, window, cx| {
                let name = this.repo.name.clone();
                window.close_dialog(cx);
                (this.shark)
                    .update(cx, |s, cx| {
                        s.state.repos.retain(|r| r.name != name);
                        s.state.repo_settings.remove(&name);
                        let _ = s.state.save();
                        cx.notify();
                    })
                    .ok();
            },
            window,
            cx,
        );
    }

    fn render_scripts(&self, phase: Phase, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (title, desc, _) = info(phase);
        let slot = phase as usize * 1000;
        let rows =
            self.scripts[phase as usize]
                .iter()
                .enumerate()
                .map(|(ix, input)| {
                    let label: SharedString = input.read(cx).value().to_string().into();
                    h_flex()
                        .id(("script", slot + ix))
                        .px_2()
                        .py_1p5()
                        .gap_2()
                        .when(ix > 0, |r| r.border_t_1().border_color(theme.border))
                        .drag_over::<ScriptDrag>(move |s, d, _, cx| match d.phase == phase {
                            true => s.bg(cx.theme().accent),
                            false => s,
                        })
                        .on_drop(cx.listener(move |this, d: &ScriptDrag, _, cx| {
                            if d.phase == phase {
                                this.move_script(d, ix, cx)
                            }
                        }))
                        .child(
                            div()
                                .w(px(14.))
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("{}", ix + 1)),
                        )
                        .child(
                            div()
                                .id(("grip", slot + ix))
                                .cursor_grab()
                                .text_color(theme.muted_foreground)
                                .child(Icon::new(IconName::GripVertical).with_size(px(14.)))
                                .on_drag(ScriptDrag { phase, ix, label }, |d, _, _, cx| {
                                    cx.new(|_| d.clone())
                                }),
                        )
                        .child(
                            div()
                                .flex_1()
                                .font_family(theme.mono_font_family.clone())
                                .text_size(theme.mono_font_size)
                                .child(Input::new(input).appearance(false).small()),
                        )
                        .child(
                            Button::new(("remove-script", slot + ix))
                                .ghost()
                                .xsmall()
                                .icon(IconName::X)
                                .tooltip("Remove")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.remove_script(phase, ix, cx)
                                })),
                        )
                });
        let list = self.scripts[phase as usize].clone();
        v_flex()
            .gap_1p5()
            .child(section(theme, title))
            .when(phase == Phase::Run, |d| {
                d.child(Self::start_order_note(theme))
            })
            .child(
                card(theme).children(rows).child(
                    h_flex()
                        .p_1()
                        .when(!list.is_empty(), |r| {
                            r.border_t_1().border_color(theme.border)
                        })
                        .child(
                            Button::new(("add-script", phase as usize))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Plus)
                                .label("Add script")
                                .on_click(cx.listener(move |this, _, w, cx| {
                                    this.add_script(phase, "", w, cx);
                                    if let Some(i) = this.scripts[phase as usize].last() {
                                        i.update(cx, |i, cx| i.focus(w, cx));
                                    }
                                    cx.notify();
                                })),
                        ),
                ),
            )
            .child(div().text_xs().text_color(faint(theme)).child(desc))
    }

    /// Run scripts are started in order, not chained: say so where the order is set.
    fn start_order_note(theme: &Theme) -> impl IntoElement {
        let code = |t: &'static str| {
            div()
                .font_family(theme.mono_font_family.clone())
                .text_color(theme.foreground)
                .child(t)
        };
        h_flex()
            .items_start()
            .gap_2()
            .p_2()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(Icon::new(IconName::Info).with_size(px(14.)).text_color(theme.foreground))
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_color(theme.foreground)
                            .font_weight(FontWeight::MEDIUM)
                            .child("Order is start order only"),
                    )
                    .child(
                        "Each script starts right after the one above it, without waiting for it \
                         to be ready or to finish. If one needs another up first, wait in its script:",
                    )
                    .child(code(
                        "until nc -z localhost $SHARK_PORT; do sleep 0.5; done; npm run dev",
                    )),
            )
    }
}

impl Render for RepoSettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (path, name) = (self.repo.path.clone(), self.repo.name.clone());
        let scripts: Vec<AnyElement> = Phase::ALL
            .into_iter()
            .map(|p| self.render_scripts(p, cx).into_any_element())
            .collect();
        let theme = cx.theme();
        let p2 = path.clone();
        let header = h_flex()
            .gap_3()
            .child(Icon::new(IconName::FolderGit2).with_size(px(20.)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(name),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(tilde(&path)),
                    ),
            )
            .child(
                Button::new("repo-reveal")
                    .ghost()
                    .small()
                    .icon(IconName::FolderOpen)
                    .tooltip("Reveal in Finder")
                    .on_click(move |_, _, cx| cx.reveal_path(&path)),
            )
            .child(
                Button::new("repo-copy-path")
                    .ghost()
                    .small()
                    .icon(IconName::Copy)
                    .tooltip("Copy path")
                    .on_click(move |_, w, cx| copy(p2.display().to_string(), w, cx)),
            )
            .child(
                Button::new("repo-new-workspace")
                    .primary()
                    .small()
                    .icon(IconName::Plus)
                    .label("New workspace")
                    .on_click(cx.listener(|this, _, w, cx| this.new_workspace(w, cx))),
            );
        v_flex()
            .id("repo-settings")
            .key_context("RepoSettings")
            .gap_4()
            .max_h(px(600.))
            .overflow_y_scroll()
            .child(header)
            .child(
                v_flex().gap_1p5().child(section(theme, "Agent")).child(
                    card(theme)
                        .child(row(
                            theme,
                            false,
                            "Default model",
                            "New chats in workspaces with this repo",
                            Select::new(&self.model).small(),
                        ))
                        .child(row(
                            theme,
                            true,
                            "Thinking",
                            "Their thinking level",
                            Select::new(&self.effort).small(),
                        )),
                ),
            )
            .children(scripts)
            .child(
                h_flex().child(
                    Button::new("remove-repo")
                        .ghost()
                        .small()
                        .label("Remove from Shark…")
                        .on_click(cx.listener(|this, _, w, cx| this.remove_repo(w, cx))),
                ),
            )
    }
}
