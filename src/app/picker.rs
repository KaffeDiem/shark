//! "Open file…": fuzzy-pick any file in the workspace's repos and open it in the editor.
use super::*;
use crate::complete::{self, Item, Kind};

const ROWS: usize = 50;

pub(super) struct FilePicker {
    state: Entity<CommandState>,
    files: Vec<Item>,
    hits: Vec<usize>,
    query: String,
    shark: WeakEntity<Shark>,
    /// The workspace and tab group it was opened from; the file opens there.
    origin: (String, usize),
    _load: Task<()>,
}

impl FilePicker {
    fn new(
        repos: Vec<(String, PathBuf)>,
        shark: WeakEntity<Shark>,
        origin: (String, usize),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let state = cx.new(|cx| CommandState::new(window, cx));
        state.update(cx, |s, cx| s.set_loading(true, window, cx));
        let _load = cx.spawn_in(window, async move |this, cx| {
            let files = cx
                .background_executor()
                .spawn(async move { complete::files(&repos) })
                .await;
            this.update_in(cx, |p, window, cx| {
                p.files = files.into_iter().filter(|i| i.kind == Kind::File).collect();
                p.rank();
                p.state.update(cx, |s, cx| s.set_loading(false, window, cx));
                cx.notify();
            })
            .ok();
        });
        FilePicker {
            state,
            files: vec![],
            hits: vec![],
            query: String::new(),
            shark,
            origin,
            _load,
        }
    }

    fn rank(&mut self) {
        self.hits = complete::rank(&self.files, &self.query, ROWS);
    }

    /// Workspace-relative path of a hit, `api/src/a.rs`.
    fn path(&self, row: usize) -> Option<String> {
        Some(rel(self.files.get(*self.hits.get(row)?)?))
    }

    /// The hit at `row` and where it opens.
    fn target(&self, row: usize) -> Option<Picked> {
        Some((self.path(row)?, self.origin.clone(), self.shark.clone()))
    }
}

type Picked = (String, (String, usize), WeakEntity<Shark>);

/// Closes the picker and opens the hit where the picker was opened from; `new_tab` is
/// cmd-enter or cmd-click.
fn open_picked(
    (path, (workspace, g), shark): Picked,
    new_tab: bool,
    window: &mut Window,
    cx: &mut App,
) {
    window.close_dialog(cx);
    shark
        .update(cx, |s, cx| {
            s.open_any_path(path, &workspace, g, new_tab, window, cx)
        })
        .ok();
}

fn rel(item: &Item) -> String {
    format!("{}/{}", item.detail, item.label)
}

impl Render for FilePicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let command_held = window.modifiers().platform;
        let dark = cx.theme().is_dark();
        let items = (self.hits.iter()).map(|&i| {
            let label: SharedString = rel(&self.files[i]).into();
            let item = CommandItem::new().label(label.clone());
            match crate::file_icon::file_icon(&self.files[i].label, dark) {
                None => item.icon(IconName::File),
                Some(icon) => item.child(move |_, _| {
                    command_row(img(icon.clone()).size_4().flex_none(), label.clone())
                }),
            }
        });
        let (q, pick) = (cx.entity().downgrade(), cx.entity().downgrade());
        let command = Command::new(&self.state)
            .items(items.collect::<Vec<_>>())
            .filterable(false)
            .placeholder("Open file…")
            .footer(move |_, _, cx| {
                let theme = cx.theme();
                let (muted, faint) = (theme.muted_foreground, faint(theme));
                h_flex()
                    .id("file-picker-hints")
                    .px_3()
                    .py_2()
                    .gap_4()
                    .text_xs()
                    .text_color(faint)
                    .child(crate::chat::hint("enter", "Open"))
                    .child(
                        crate::chat::hint("cmd-enter", "New tab")
                            .id("new-tab-hint")
                            .when(command_held, |d| {
                                d.text_color(muted).font_weight(FontWeight::MEDIUM)
                            })
                            .test_support(),
                    )
                    .child(crate::chat::hint("escape", "Cancel"))
                    .test_support()
            })
            .on_query(move |query, _, cx| {
                q.update(cx, |p, cx| {
                    p.query = query.to_string();
                    p.rank();
                    cx.notify();
                })
                .ok();
            })
            .on_confirm(move |ix, window, cx| {
                // Enter or a click; cmd-click (the click's modifiers) appends.
                let new_tab = window.modifiers().platform;
                // Closing the dialog releases the picker, so read the selection first.
                if let Some(picked) = pick.upgrade().and_then(|p| p.read(cx).target(ix.row)) {
                    open_picked(picked, new_tab, window, cx);
                }
            });
        div()
            .key_context("FilePicker")
            .on_modifiers_changed(cx.listener(|_, _: &ModifiersChangedEvent, _, cx| cx.notify()))
            .on_action(cx.listener(|p, _: &OpenInNewTab, window, cx| {
                let row = p.state.read(cx).selected_index().map(|ix| ix.row);
                if let Some(picked) = row.and_then(|row| p.target(row)) {
                    window.defer(cx, move |window, cx| open_picked(picked, true, window, cx));
                }
            }))
            .child(command)
    }
}

impl Shark {
    /// cmd-p: the picker over every tracked and untracked file of the open workspace.
    pub(super) fn open_any_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.active_ws() else {
            return notify_err(window, cx, "Open a workspace first");
        };
        let repos = palette::repo_paths(ws);
        let origin = (ws.name.clone(), ws.layout.focused);
        let shark = cx.entity().downgrade();
        let picker = cx.new(|cx| FilePicker::new(repos, shark, origin, window, cx));
        let state = picker.read(cx).state.clone();
        window.open_dialog(cx, move |d, _, cx| menu_dialog(d, cx).child(picker.clone()));
        state.update(cx, |p, cx| p.focus(window, cx));
    }

    /// Opens a picked path in group `g` of `workspace`; text files only, since the editor would
    /// show a binary one empty. An open path just gets its tab selected.
    fn open_any_path(
        &mut self,
        path: String,
        workspace: &str,
        g: usize,
        new_tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.active_ws().filter(|ws| ws.name == workspace) else {
            return notify_err(window, cx, "The workspace changed; open the file again");
        };
        let abs = ws.dir().join(&path);
        let text = std::fs::metadata(&abs).is_ok_and(|m| m.len() <= crate::editor::MAX_BYTES)
            && std::fs::read(&abs).is_ok_and(|b| std::str::from_utf8(&b).is_ok());
        if !text {
            return notify_err(
                window,
                cx,
                format!(
                    "{path} is binary, too large or unreadable; open it in Finder (Reveal in Finder in cmd-k)"
                ),
            );
        }
        self.open_in_group(path, workspace, g, new_tab, window, cx);
    }
}
