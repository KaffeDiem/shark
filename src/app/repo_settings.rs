//! Repo settings (click a repo in the sidebar): new workspace, default model, setup, run and
//! archive scripts, removal. Every change saves at once, as in Settings.
use super::*;
use crate::chat::{effort_label, efforts, model_label};
use crate::settings::{card, row, section};
use crate::state::RepoSettings;
use gpui_kit::component::IndexPath;
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};

/// Title, description and placeholder, in `RepoSettings` order: setup, run, archive.
const PHASES: [(&str, &str, &str); 3] = [
    (
        "Setup",
        "Run in each new worktree of this repo, in order",
        "npm install  or  cp $SHARK_ROOT_PATH/.env .",
    ),
    (
        "Run",
        "cmd-r starts these and stops them; across repos, drag the repos in the sidebar. $SHARK_PORT is a free block of 10 ports",
        "npm run dev -- --port $SHARK_PORT",
    ),
    (
        "Archive",
        "Run before the worktree is removed",
        "docker compose down",
    ),
];

type Choice = Entity<SelectState<SearchableVec<SharedString>>>;

/// A script row being dragged by its grip: which list and where from.
#[derive(Clone)]
struct ScriptDrag {
    phase: usize,
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
    model: Choice,
    effort: Choice,
    /// One input per script, per `PHASES` entry; saved on every change.
    scripts: [Vec<Entity<InputState>>; 3],
    _subs: Vec<Subscription>,
}

pub(super) fn open(shark: &mut Shark, repo: Repo, window: &mut Window, cx: &mut Context<Shark>) {
    let settings = shark.state.repo_settings.get(&repo.name).cloned();
    let weak = cx.entity().downgrade();
    let view =
        cx.new(|cx| RepoSettingsView::new(weak, repo, settings.unwrap_or_default(), window, cx));
    window.open_dialog(cx, move |d, _, _| {
        d.font_features(inter_features())
            .entrance(DialogEntrance::None)
            .w(px(640.))
            .child(view.clone())
    });
}

impl RepoSettingsView {
    fn new(
        shark: WeakEntity<Shark>,
        repo: Repo,
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
        let effort = Self::effort_choice(&s, window, cx);
        let mut subs = vec![cx.subscribe_in(&model, window, |this, _, ev, window, cx| {
            let SelectEvent::Confirm(Some(v)) = ev else {
                return;
            };
            let m = MODELS.iter().find(|m| model_label(m) == v.as_ref());
            let s = this.update(cx, |s| {
                s.model = m.map(|m| m.to_string());
                let levels = efforts(s.model.as_deref().unwrap_or(MODELS[0]));
                s.effort = s.effort.take().filter(|e| levels.contains(&e.as_str()));
            });
            // Claude and Codex take different levels.
            this.effort = Self::effort_choice(&s, window, cx);
            this._subs.push(this.watch_effort(window, cx));
        })];
        let mut this = RepoSettingsView {
            shark,
            repo,
            model,
            effort,
            scripts: Default::default(),
            _subs: vec![],
        };
        subs.push(this.watch_effort(window, cx));
        for (p, list) in [s.setup, s.run, s.archive].into_iter().enumerate() {
            for script in list {
                this.add_script(p, &script, window, cx);
            }
        }
        this._subs.extend(subs);
        this
    }

    fn effort_choice(s: &RepoSettings, window: &mut Window, cx: &mut Context<Self>) -> Choice {
        let levels = efforts(s.model.as_deref().unwrap_or(MODELS[0]));
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
            this.update(cx, move |s| {
                let levels = efforts(s.model.as_deref().unwrap_or(MODELS[0]));
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
            [s.setup, s.run, s.archive] = [lists[0].clone(), lists[1].clone(), lists[2].clone()];
        });
    }

    fn add_script(
        &mut self,
        phase: usize,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let hint = PHASES[phase].2;
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
        self.scripts[phase].push(input);
    }

    fn remove_script(&mut self, phase: usize, ix: usize, cx: &mut Context<Self>) {
        self.scripts[phase].remove(ix);
        self.save_scripts(cx);
    }

    fn move_script(&mut self, d: &ScriptDrag, to: usize, cx: &mut Context<Self>) {
        let list = &mut self.scripts[d.phase];
        if d.ix < list.len() && to < list.len() {
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
                (s.read(cx).state.workspaces.iter())
                    .filter(|w| w.repos.iter().any(|r| r.name == name))
                    .map(|w| w.name.clone())
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

    fn render_scripts(&self, phase: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (title, desc, _) = PHASES[phase];
        let rows = self.scripts[phase].iter().enumerate().map(|(ix, input)| {
            let label: SharedString = input.read(cx).value().to_string().into();
            h_flex()
                .id(("script", phase * 1000 + ix))
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
                        .id(("grip", phase * 1000 + ix))
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
                    Button::new(("remove-script", phase * 1000 + ix))
                        .ghost()
                        .xsmall()
                        .icon(IconName::X)
                        .tooltip("Remove")
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.remove_script(phase, ix, cx)),
                        ),
                )
        });
        let list = self.scripts[phase].clone();
        v_flex()
            .gap_1p5()
            .child(section(theme, title))
            .when(phase == 1, |d| d.child(Self::start_order_note(theme)))
            .child(
                card(theme).children(rows).child(
                    h_flex()
                        .p_1()
                        .when(!list.is_empty(), |r| {
                            r.border_t_1().border_color(theme.border)
                        })
                        .child(
                            Button::new(("add-script", phase))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Plus)
                                .label("Add script")
                                .on_click(cx.listener(move |this, _, w, cx| {
                                    this.add_script(phase, "", w, cx);
                                    if let Some(i) = this.scripts[phase].last() {
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
        let scripts: Vec<AnyElement> = (0..PHASES.len())
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
