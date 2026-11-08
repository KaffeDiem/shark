//! The textarea and completions shared by Home and agent task drafts.
use crate::complete::{self, Item, Kind};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{
    Enter, Escape, IndentInline, InlineToken, InputEvent, MoveDown, MoveUp, OutdentInline,
    Textarea, TextareaState,
};
use gpui_kit::component::{ActiveTheme, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::PathBuf;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Source {
    pub name: String,
    pub path: PathBuf,
    pub start_ref: String,
    pub new_branch: bool,
    pub pr: Option<crate::git::PullRequest>,
}

pub(crate) enum Event {
    Submit,
    Change,
    Escape,
    TogglePlan,
    ToggleAsk,
    ModelMenu,
    Tray,
    AgentMove(isize),
    Attach {
        paths: Vec<PathBuf>,
        image: Option<(Vec<u8>, &'static str)>,
    },
}

struct Menu {
    range: std::ops::Range<usize>,
    source: char,
    matches: Vec<usize>,
    selected: usize,
    armed: bool,
}

pub(crate) struct TaskComposer {
    pub input: Entity<TextareaState>,
    sources: Vec<Source>,
    skills: Vec<String>,
    repo_skills: bool,
    codex_skills: bool,
    home_agents: bool,
    home_agent: Option<String>,
    asking: bool,
    files: Vec<Item>,
    prs: Vec<Item>,
    commands: Vec<Item>,
    menu: Option<Menu>,
    scroll: ScrollHandle,
    loading: bool,
    file_error: Option<String>,
    pr_error: Option<String>,
    loading_prs: bool,
    generation: u64,
    read_only: bool,
    dropping: Option<Vec<PathBuf>>,
    _subscription: Subscription,
}

impl EventEmitter<Event> for TaskComposer {}

/// Keep submission/newline behavior identical in chat and task drafts.
pub(crate) fn textarea(
    window: &mut Window,
    cx: &mut Context<TextareaState>,
    rows: usize,
) -> TextareaState {
    TextareaState::new(window, cx)
        .auto_grow(rows, 10)
        .submit_on_enter(true)
}

pub(crate) fn insert_token(
    input: &Entity<TextareaState>,
    text: String,
    label: String,
    window: &mut Window,
    cx: &mut App,
) {
    input.update(cx, |input, cx| {
        let token = InlineToken::new(text.clone(), text.clone()).with_label(label);
        if input.replace_with_token(token, window, cx).is_err() {
            // shortcut: control characters cannot form tokens, preserve their plain text instead.
            input.replace(text, window, cx);
        }
        input.insert(" ", window, cx);
    });
}

impl TaskComposer {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| textarea(window, cx, 2));
        let subscription =
            cx.subscribe_in(
                &input,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::PressEnter { shift: false, .. } if !this.read_only => {
                        cx.emit(Event::Submit)
                    }
                    InputEvent::Change => {
                        this.update_menu(cx);
                        cx.emit(Event::Change);
                    }
                    InputEvent::Blur => this.close_menu(cx),
                    _ => {}
                },
            );
        Self {
            input,
            sources: vec![],
            skills: vec![],
            repo_skills: crate::git::repo_skills(),
            codex_skills: false,
            home_agents: false,
            home_agent: None,
            asking: false,
            files: vec![],
            prs: vec![],
            commands: vec![],
            menu: None,
            scroll: ScrollHandle::new(),
            loading: false,
            file_error: None,
            pr_error: None,
            loading_prs: false,
            generation: 0,
            read_only: false,
            dropping: None,
            _subscription: subscription,
        }
    }

    pub(crate) fn set_read_only(&mut self, read_only: bool, cx: &mut Context<Self>) {
        if self.read_only != read_only {
            self.read_only = read_only;
            self.input
                .update(cx, |input, cx| input.set_readonly(read_only, cx));
            if read_only {
                self.dropping = None;
                self.close_menu(cx);
            }
            cx.notify();
        }
    }

    pub(crate) fn set_sources(
        &mut self,
        sources: Vec<Source>,
        skills: Vec<String>,
        home_agents: bool,
        codex: bool,
        cx: &mut Context<Self>,
    ) {
        let repo_skills = crate::git::repo_skills();
        let changed = self.sources != sources
            || self.skills != skills
            || self.repo_skills != repo_skills
            || self.codex_skills != codex;
        self.home_agents = home_agents;
        if !changed {
            return;
        }
        self.repo_skills = repo_skills;
        self.codex_skills = codex;
        self.sources = sources;
        self.skills = skills;
        self.files.clear();
        self.prs = complete::pr_items(
            &self
                .sources
                .iter()
                .flat_map(|s| complete::cached_repo_prs(&s.name, &s.path, false))
                .collect::<Vec<_>>(),
        );
        self.commands = complete::skill_items(self.skills.clone());
        self.close_menu(cx);
        self.generation += 1;
        let generation = self.generation;
        let sources = self.sources.clone();
        let skills = self.skills.clone();
        self.loading = true;
        self.file_error = None;
        self.pr_error = None;
        self.loading_prs = !cfg!(test) && !self.sources.is_empty();
        let pr_sources = sources.clone();
        cx.spawn(async move |this, cx| {
            let (files, skills) = cx
                .background_executor()
                .spawn(async move {
                    let repos: Result<Vec<_>, String> = sources
                        .iter()
                        .map(|source| {
                            let reference = if let Some(pr) = &source.pr {
                                crate::git::fetch_composer_pr_head(&source.path, pr)?
                            } else {
                                source.start_ref.clone()
                            };
                            Ok((
                                source.name.clone(),
                                source.path.clone(),
                                reference,
                                source.new_branch,
                            ))
                        })
                        .collect();
                    match repos.and_then(|repos| complete::committed_paths(&repos)) {
                        Ok(repos) => {
                            let files = complete::file_items(&repos.files);
                            let repos = if repo_skills { repos.skills } else { vec![] };
                            let skills = complete::draft_skill_names(&skills, &repos, codex);
                            (Ok(files), skills)
                        }
                        Err(error) => (Err(error), skills),
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match files {
                    Ok(files) => this.files = files,
                    Err(error) => this.file_error = Some(error),
                }
                this.commands = complete::skill_items(skills);
                if this.has_menu() {
                    this.update_menu(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        if !cfg!(test) {
            cx.spawn(async move |this, cx| {
                let results = cx
                    .background_executor()
                    .spawn(async move {
                        crate::proc::par_map(&pr_sources, |source| {
                            match complete::fetch_repo_prs(&source.name, &source.path, false) {
                                Ok(found) => (found, None),
                                Err(error) => (
                                    complete::cached_repo_prs(&source.name, &source.path, false),
                                    Some(format!("{}: {error}", source.name)),
                                ),
                            }
                        })
                    })
                    .await;
                this.update(cx, |this, cx| {
                    if this.generation != generation {
                        return;
                    }
                    let mut prs = vec![];
                    let mut errors = vec![];
                    for (found, error) in results {
                        prs.extend(found);
                        errors.extend(error);
                    }
                    this.loading_prs = false;
                    this.pr_error = (!errors.is_empty()).then(|| errors.join(" · "));
                    this.refresh_prs(complete::pr_items(&prs), cx);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    fn refresh_prs(&mut self, prs: Vec<Item>, cx: &mut Context<Self>) {
        let selection = self
            .menu
            .as_ref()
            .filter(|menu| menu.source == '#')
            .and_then(|menu| {
                let item = self.prs.get(*menu.matches.get(menu.selected)?)?;
                Some((item.insert.clone(), menu.armed))
            });
        self.prs = prs;
        if !self.has_menu() {
            return;
        }
        self.update_menu(cx);
        if let Some((url, armed)) = selection
            && let Some(menu) = self.menu.as_mut().filter(|menu| menu.source == '#')
        {
            let selected = menu.matches.iter().position(|&i| self.prs[i].insert == url);
            menu.armed = selected.is_some() && armed;
            if let Some(selected) = selected {
                menu.selected = selected;
                self.scroll.scroll_to_item(selected);
            }
        }
    }

    pub(crate) fn set_asking(&mut self, asking: bool, cx: &mut Context<Self>) {
        self.asking = asking;
        if asking {
            self.close_menu(cx);
        }
    }

    pub(crate) fn set_home_agent(&mut self, agent: Option<String>, cx: &mut Context<Self>) {
        if self.home_agent != agent {
            self.home_agent = agent;
            self.update_menu(cx);
        }
    }

    fn completing_agent(&self, cx: &App) -> bool {
        self.home_agents
            && !self.asking
            && self
                .input
                .read(cx)
                .value()
                .strip_prefix('@')
                .is_some_and(|query| !query.contains(char::is_whitespace))
    }

    fn source(&self, source: char) -> &[Item] {
        match source {
            '/' => &self.commands,
            '#' => &self.prs,
            _ => &self.files,
        }
    }

    pub(crate) fn has_menu(&self) -> bool {
        self.menu.is_some()
    }

    pub(crate) fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    fn update_menu(&mut self, cx: &mut Context<Self>) {
        if self.repo_skills != crate::git::repo_skills() {
            self.set_sources(
                self.sources.clone(),
                self.skills.clone(),
                self.home_agents,
                self.codex_skills,
                cx,
            );
        }
        if self.asking {
            return self.close_menu(cx);
        }
        let input = self.input.read(cx);
        let text = input.value();
        let offset = self
            .home_agent
            .as_ref()
            .and_then(|agent| {
                let prefix = format!("@{agent}");
                let body = text.trim_start();
                body.strip_prefix(&prefix)
                    .filter(|body| body.starts_with(char::is_whitespace))
                    .map(|_| text.len() - body.len() + prefix.len())
            })
            .filter(|offset| *offset <= input.cursor())
            .unwrap_or(0);
        let trigger = complete::trigger(&text[offset..], input.cursor() - offset)
            .map(|(range, source)| (range.start + offset..range.end + offset, source))
            .filter(|(range, source)| {
                !(self.home_agents && *source == '@' && range.start == 0)
                    && !input.tokens().iter().any(|token| {
                        token.range().start < range.end && range.start < token.range().end
                    })
            });
        let Some((range, source)) = trigger else {
            return self.close_menu(cx);
        };
        let query = &text[range.start + 1..range.end];
        let items = self.source(source);
        let matches = if source == '#' {
            complete::rank_prs(items, query, 50)
        } else {
            complete::rank(items, query, 50)
        };
        let (selected, armed) = self
            .menu
            .as_ref()
            .filter(|m| m.source == source && m.matches == matches)
            .map_or((0, false), |m| (m.selected, m.armed));
        let armed = source != '#'
            || armed
            || matches
                .first()
                .is_some_and(|i| complete::names_pr(&items[*i], query));
        self.menu = Some(Menu {
            range,
            source,
            matches,
            selected,
            armed,
        });
        self.scroll.scroll_to_item(selected);
        cx.notify();
    }

    fn move_menu(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(menu) = &mut self.menu
            && !menu.matches.is_empty()
        {
            menu.selected =
                (menu.selected as isize + delta).rem_euclid(menu.matches.len() as isize) as usize;
            menu.armed = true;
            self.scroll.scroll_to_item(menu.selected);
            cx.notify();
        }
    }

    fn pick(
        &mut self,
        selected: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(menu) = self.menu.as_ref() else {
            return false;
        };
        if selected.is_none() && !menu.armed {
            return false;
        }
        let Some(index) = menu.matches.get(selected.unwrap_or(menu.selected)) else {
            return false;
        };
        let item = self.source(menu.source)[*index].clone();
        let range = self.menu.take().unwrap().range;
        self.input
            .update(cx, |input, cx| input.set_selected_range(range, cx));
        let label = if self.sources.len() > 1 && item.kind != Kind::Command {
            format!("{} · {}", item.label, item.badge)
        } else {
            item.label
        };
        insert_token(&self.input, item.insert, label, window, cx);
        self.close_menu(cx);
        true
    }

    fn paste(&mut self, item: &ClipboardItem, cx: &mut Context<Self>) -> bool {
        if self.read_only {
            return true;
        }
        if self.asking {
            return false;
        }
        if let Some(paths) = item.entries().iter().find_map(|e| match e {
            ClipboardEntry::ExternalPaths(paths) => Some(paths.paths().to_vec()),
            _ => None,
        }) {
            cx.emit(Event::Attach { paths, image: None });
            return true;
        }
        if item
            .entries()
            .iter()
            .any(|e| matches!(e, ClipboardEntry::String(_)))
        {
            return false;
        }
        if let Some(image) = item.entries().iter().find_map(|e| match e {
            ClipboardEntry::Image(image) => Some(image),
            _ => None,
        }) {
            cx.emit(Event::Attach {
                paths: vec![],
                image: Some((image.bytes.clone(), image.format.extension())),
            });
            return true;
        }
        false
    }

    fn render_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.menu.as_ref()?;
        let theme = cx.theme();
        let rows = menu
            .matches
            .iter()
            .enumerate()
            .map(|(selected, index)| {
                let item = &self.source(menu.source)[*index];
                let icon = match item.kind {
                    Kind::File => IconName::File,
                    Kind::Dir => IconName::Folder,
                    Kind::Command => IconName::Slash,
                    Kind::Pr(_) => IconName::GitPullRequest,
                };
                h_flex()
                    .id(selected)
                    .h(crate::menu::ROW_H)
                    .px_2()
                    .gap_2()
                    .rounded(theme.radius)
                    .when(selected == menu.selected && menu.armed, |d| {
                        d.bg(crate::menu::highlight(theme))
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.pick(Some(selected), window, cx);
                            window.prevent_default();
                        }),
                    )
                    .child(Icon::new(icon).with_size(px(16.)))
                    .child(item.label.clone())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(item.detail.clone()),
                    )
                    .when(self.sources.len() > 1 && item.kind != Kind::Command, |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(item.badge.clone()),
                        )
                    })
            })
            .collect::<Vec<_>>();
        let error = match menu.source {
            '#' => &self.pr_error,
            '@' => &self.file_error,
            _ => &None,
        };
        let loading = if menu.source == '#' {
            self.loading_prs
        } else {
            self.loading
        };
        let empty = if self.sources.is_empty() && menu.source != '/' {
            "Add a repository to reference files"
        } else if loading {
            "Loading…"
        } else if error.is_some() {
            "Couldn't load references"
        } else {
            match menu.source {
                '/' => "No matching skills",
                '#' => "No matching pull requests",
                _ => "No matching files",
            }
        };
        Some(
            crate::menu::chrome(theme)
                .id("task-completions")
                .absolute()
                .top_full()
                .left_0()
                .right_0()
                .mt_1()
                .occlude()
                .child(
                    div()
                        .id("task-completion-rows")
                        .max_h(crate::menu::ROW_H * 8.)
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .when(rows.is_empty(), |d| {
                            d.child(
                                div()
                                    .px_2()
                                    .py_2()
                                    .text_color(theme.muted_foreground)
                                    .child(empty),
                            )
                        })
                        .children(rows),
                )
                .when_some(error.clone(), |d, error| {
                    d.child(
                        div()
                            .px_2()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(error),
                    )
                })
                .into_any_element(),
        )
    }
}

impl Render for TaskComposer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let dropped = this.clone();
        v_flex()
            .id("task-composer-input")
            .test_support()
            .relative()
            .w_full()
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "/"
                    && event.keystroke.modifiers.platform
                    && !this.read_only
                    && !this.asking
                {
                    cx.emit(Event::ModelMenu);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                if this.read_only {
                    return;
                }
                if this.has_menu() {
                    this.move_menu(1, cx);
                    cx.stop_propagation();
                } else if this.completing_agent(cx) {
                    cx.emit(Event::AgentMove(1));
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                if this.read_only {
                    return;
                }
                if this.has_menu() {
                    this.move_menu(-1, cx);
                    cx.stop_propagation();
                } else if this.completing_agent(cx) {
                    cx.emit(Event::AgentMove(-1));
                    cx.stop_propagation();
                } else if !this.asking && {
                    let input = this.input.read(cx);
                    input.cursor() == 0 && input.value().split('\n').next() == Some("")
                } {
                    cx.emit(Event::Tray);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, event: &Enter, window, cx| {
                if !event.shift && this.pick(None, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                if !this.read_only {
                    if let Some(menu) = &this.menu {
                        this.pick(Some(menu.selected), window, cx);
                    } else if this.home_agents {
                        cx.emit(Event::ToggleAsk);
                    }
                }
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &OutdentInline, _, cx| {
                if !this.read_only && !this.asking {
                    cx.emit(Event::TogglePlan);
                }
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                if this.has_menu() {
                    this.close_menu(cx);
                } else {
                    cx.emit(Event::Escape);
                }
                cx.stop_propagation();
            }))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<ExternalPaths>, _, cx| {
                    this.dropping = (!this.read_only
                        && !this.asking
                        && event.bounds.contains(&event.event.position))
                    .then(|| event.drag(cx).paths().to_vec());
                }),
            )
            .on_file_drop_exit(cx.listener(|this, _, _, _| this.dropping = None))
            .child(
                canvas(
                    |_, _, _| {},
                    move |_, _, window, _| {
                        let dropped = dropped.clone();
                        window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                            if phase == DispatchPhase::Bubble {
                                dropped
                                    .update(cx, |this, cx| {
                                        if let Some(paths) = this.dropping.take() {
                                            cx.emit(Event::Attach { paths, image: None });
                                        }
                                    })
                                    .ok();
                            }
                        });
                    },
                )
                .absolute()
                .size_0(),
            )
            .child(
                Textarea::new(&self.input)
                    .appearance(false)
                    .readonly(self.read_only)
                    .on_paste(move |item, _, cx| {
                        this.update(cx, |this, cx| this.paste(item, cx))
                            .unwrap_or(false)
                    }),
            )
            .children(
                self.render_menu(cx)
                    .map(|menu| deferred(menu).with_priority(100)),
            )
    }
}

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use super::{Event, TaskComposer};
    use core::prelude::v1::test;
    use gpui_kit::InputEvent as _;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AppContext as _, Context, ExternalPaths, FileDropEvent, Keystroke, TestAppContext, Window,
        WindowOptions,
    };
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

    #[gpui_kit::test]
    fn multiline_completion_and_home_keys(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::bind_keys(cx);
        });
        let (window, composer) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                    cx.new(|cx| TaskComposer::new(window, cx))
                })
            })
            .unwrap();
        let events = Rc::new(RefCell::new(Vec::new()));
        let seen = events.clone();
        let _subscription = cx.update(|cx| {
            cx.subscribe(&composer, move |_, event: &Event, _| {
                seen.borrow_mut().push(match event {
                    Event::Submit => "submit",
                    Event::ToggleAsk => "ask",
                    Event::Escape => "escape",
                    Event::TogglePlan => "plan",
                    Event::AgentMove(_) => "agent",
                    _ => "other",
                });
            })
        });
        cx.run_until_parked();
        let act =
            |cx: &mut TestAppContext,
             f: &dyn Fn(&mut TaskComposer, &mut Window, &mut Context<TaskComposer>)| {
                cx.update_window(window, |_, window, cx| {
                    composer.update(cx, |this, cx| f(this, window, cx))
                })
                .unwrap();
            };
        let press = |cx: &mut TestAppContext, key: &str| {
            cx.update_window(window, |_, window, cx| window.press(key, cx))
                .unwrap();
        };
        act(cx, &|this, window, cx| {
            this.set_sources(vec![], vec!["review".into()], true, false, cx);
            this.input.update(cx, |input, cx| {
                input.set_value("", window, cx);
                input.insert("first", window, cx);
                input.focus(window, cx);
            });
        });
        cx.run_until_parked();
        press(cx, "shift-enter");
        cx.update(|cx| assert_eq!(composer.read(cx).input.read(cx).value().as_ref(), "first\n"));
        assert!(!events.borrow().contains(&"submit"));
        press(cx, "enter");
        assert_eq!(
            events.borrow().iter().filter(|e| **e == "submit").count(),
            1
        );
        act(cx, &|this, window, cx| {
            this.input.update(cx, |input, cx| {
                input.set_value("", window, cx);
                input.insert("/rev", window, cx);
            })
        });
        assert!(cx.update(|cx| composer.read(cx).has_menu()));
        press(cx, "enter");
        cx.update(|cx| {
            let input = composer.read(cx).input.read(cx);
            assert_eq!(input.value().as_ref(), "/review ");
            assert_eq!(input.tokens().len(), 1);
        });
        assert_eq!(
            events.borrow().iter().filter(|e| **e == "submit").count(),
            1,
            "completion cannot start a task"
        );
        act(cx, &|this, window, cx| {
            this.input.update(cx, |input, cx| {
                input.set_value("", window, cx);
                input.insert("see @", window, cx);
            })
        });
        assert!(
            cx.update(|cx| composer.read(cx).has_menu()),
            "empty repositories still show the help row"
        );
        press(cx, "tab");
        assert!(
            !events.borrow().contains(&"ask"),
            "an empty completion menu cannot toggle Ask"
        );
        press(cx, "escape");
        assert!(!cx.update(|cx| composer.read(cx).has_menu()));
        assert!(
            !events.borrow().contains(&"escape"),
            "first escape closes completion only"
        );
        press(cx, "escape");
        assert!(events.borrow().contains(&"escape"));
        act(cx, &|this, window, cx| {
            this.input.update(cx, |input, cx| {
                input.set_value("", window, cx);
                input.insert("@lin", window, cx);
            })
        });
        assert!(
            !cx.update(|cx| composer.read(cx).has_menu()),
            "Home owns the leading agent mention"
        );
        press(cx, "down");
        assert!(events.borrow().contains(&"agent"));
        press(cx, "tab");
        assert!(
            events.borrow().contains(&"ask"),
            "parent can complete an agent or toggle Ask"
        );
        for (typed, expected) in [
            ("@lin /rev", "@lin /review "),
            (" \n @lin /rev", " \n @lin /review "),
        ] {
            act(cx, &|this, window, cx| {
                this.set_home_agent(Some("lin".into()), cx);
                this.input.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                    input.insert(typed, window, cx);
                });
            });
            assert!(
                cx.update(|cx| composer.read(cx).has_menu()),
                "slash starts at the task body after the selected agent"
            );
            press(cx, "enter");
            cx.update(|cx| {
                let input = composer.read(cx).input.read(cx);
                assert_eq!(input.value().as_ref(), expected);
                assert_eq!(input.tokens().len(), 1);
            });
        }
        act(cx, &|this, window, cx| {
            this.set_asking(true, cx);
            this.input.update(cx, |input, cx| {
                input.set_value("", window, cx);
                input.insert("/review", window, cx);
            });
        });
        assert!(!cx.update(|cx| composer.read(cx).has_menu()));
    }
    #[gpui_kit::test]
    fn pr_completion_loading_preserves_review_refs(cx: &mut TestAppContext) {
        let root =
            std::env::temp_dir().join(format!("shark-composer-review-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("source");
        let remote = root.join("upstream.git");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| crate::git::git(&repo, args).unwrap();
        git(&["init", "-q"]);
        git(&["init", "--bare", "-q", remote.to_str().unwrap()]);
        let commit = |name: &str| {
            std::fs::write(repo.join(name), name).unwrap();
            git(&["add", "."]);
            git(&[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                name,
            ]);
            git(&["rev-parse", "HEAD"])
        };
        let baseline = commit("original.md");
        git(&["update-ref", &crate::git::review_ref(7, "base"), &baseline]);
        git(&["update-ref", &crate::git::review_ref(7, "head"), &baseline]);
        commit("base.md");
        git(&[
            "push",
            "-q",
            remote.to_str().unwrap(),
            "HEAD:refs/heads/release",
        ]);
        commit("head.md");
        git(&[
            "push",
            "-q",
            remote.to_str().unwrap(),
            "HEAD:refs/pull/7/head",
        ]);
        git(&[
            "config",
            &format!("url.{}.insteadOf", remote.display()),
            "https://github.com/o/upstream.git",
        ]);
        cx.update(gpui_kit::init);
        let (_, composer) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                    cx.new(|cx| TaskComposer::new(window, cx))
                })
            })
            .unwrap();
        cx.update(|cx| {
            composer.update(cx, |this, cx| {
                this.set_sources(
                    vec![super::Source {
                        name: "api".into(),
                        path: repo.clone(),
                        start_ref: String::new(),
                        new_branch: false,
                        pr: Some(crate::git::PullRequest {
                            number: 7,
                            url: "https://github.com/o/upstream/pull/7".into(),
                            base: "release".into(),
                        }),
                    }],
                    vec![],
                    false,
                    false,
                    cx,
                );
            })
        });
        cx.run_until_parked();
        cx.update(|cx| {
            let composer = composer.read(cx);
            assert!(!composer.loading, "source completion loading must finish");
            assert!(
                composer
                    .files
                    .iter()
                    .any(|item| item.insert == "@api/head.md"),
                "{:?}",
                composer.file_error
            );
        });
        assert_eq!(
            git(&["rev-parse", &crate::git::review_ref(7, "head")]),
            baseline,
            "completion must preserve review head baseline"
        );
        assert_eq!(
            git(&["rev-parse", &crate::git::review_ref(7, "base")]),
            baseline,
            "completion must preserve review base baseline"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui_kit::test]
    fn pr_refresh_preserves_selected_identity_and_disarms_removed_pr(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (window, composer) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                    cx.new(|cx| TaskComposer::new(window, cx))
                })
            })
            .unwrap();
        let rows = |numbers: &[u64]| {
            crate::complete::pr_items(
                &numbers
                    .iter()
                    .enumerate()
                    .map(|(i, &number)| crate::prs::Pr {
                        repo: "api".into(),
                        number,
                        title: "Fix".into(),
                        author: None,
                        head_ref_name: String::new(),
                        base_ref_name: String::new(),
                        url: format!("https://github.com/o/api/pull/{number}"),
                        state: "OPEN".into(),
                        is_draft: false,
                        updated_at: format!("{}", 9 - i),
                        is_cross_repository: false,
                    })
                    .collect::<Vec<_>>(),
            )
        };
        for (refreshed, expected) in [
            (vec![2, 1], Some("https://github.com/o/api/pull/2 ")),
            (vec![3, 1], None),
        ] {
            cx.update_window(window, |_, window, cx| {
                composer.update(cx, |this, cx| {
                    this.prs = rows(&[1, 2]);
                    this.close_menu(cx);
                    this.input.update(cx, |input, cx| {
                        input.set_value("", window, cx);
                        input.insert("#", window, cx);
                    });
                    this.update_menu(cx);
                    this.move_menu(1, cx);
                    this.refresh_prs(rows(&refreshed), cx);
                    let picked = this.pick(None, window, cx);
                    assert_eq!(
                        picked,
                        expected.is_some(),
                        "removed selection must be disarmed"
                    );
                    assert_eq!(
                        this.input.read(cx).value().as_ref(),
                        expected.unwrap_or("#")
                    );
                });
            })
            .unwrap();
        }
    }

    #[gpui_kit::test]
    fn file_drop_after_typing_emits_attachment(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::bind_keys(cx);
        });
        let (window, composer) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                    cx.new(|cx| TaskComposer::new(window, cx))
                })
            })
            .unwrap();
        let paths = Rc::new(RefCell::new(Vec::new()));
        let seen = paths.clone();
        let _subscription = cx.update(|cx| {
            cx.subscribe(&composer, move |_, event: &Event, _| {
                if let Event::Attach { paths, .. } = event {
                    seen.borrow_mut().extend(paths.clone());
                }
            })
        });
        cx.run_until_parked();
        let path = PathBuf::from("/tmp/draft-screenshot.png");
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let position = window.find("task-composer-input").bounds().center();
            window.dispatch_keystroke(Keystroke::parse("a").unwrap(), cx);
            window.dispatch_event(
                FileDropEvent::Entered {
                    position,
                    paths: ExternalPaths(vec![path.clone()].into()),
                }
                .to_platform_input(),
                cx,
            );
            window.dispatch_event(FileDropEvent::Pending { position }.to_platform_input(), cx);
            assert!(composer.read(cx).dropping.is_some());
            window.dispatch_event(FileDropEvent::Submit { position }.to_platform_input(), cx);
        })
        .unwrap();
        assert_eq!(*paths.borrow(), [path]);
        cx.update(|cx| assert!(composer.read(cx).dropping.is_none()));
    }
}
