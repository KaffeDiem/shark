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
    _load: Task<()>,
}

impl FilePicker {
    fn new(
        repos: Vec<(String, PathBuf)>,
        shark: WeakEntity<Shark>,
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
}

fn rel(item: &Item) -> String {
    format!("{}/{}", item.detail, item.label)
}

impl Render for FilePicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dark = cx.theme().is_dark();
        let items = (self.hits.iter()).map(|&i| {
            let label: SharedString = rel(&self.files[i]).into();
            let item = CommandItem::new().label(label.clone());
            match crate::file_icon::file_icon(&self.files[i].label, dark) {
                None => item.icon(IconName::File),
                // As `app_item` in the palette: a custom row for an image icon.
                Some(icon) => item.child(move |_, _| {
                    h_flex()
                        .flex_1()
                        .gap_2()
                        .items_center()
                        .child(img(icon.clone()).size_4().flex_none())
                        .child(div().flex_1().min_w_0().truncate().child(label.clone()))
                }),
            }
        });
        let (q, pick) = (cx.entity().downgrade(), cx.entity().downgrade());
        Command::new(&self.state)
            .items(items.collect::<Vec<_>>())
            .filterable(false)
            .placeholder("Open file…")
            .on_query(move |query, _, cx| {
                q.update(cx, |p, cx| {
                    p.query = query.to_string();
                    p.rank();
                    cx.notify();
                })
                .ok();
            })
            .on_confirm(move |ix, window, cx| {
                // Closing the dialog releases the picker, so read the selection first.
                let Some((path, shark)) = pick
                    .upgrade()
                    .and_then(|p| Some((p.read(cx).path(ix.row)?, p.read(cx).shark.clone())))
                else {
                    return;
                };
                window.close_dialog(cx);
                shark
                    .update(cx, |s, cx| s.open_any_path(path, window, cx))
                    .ok();
            })
    }
}

impl Shark {
    /// cmd-p: the picker over every tracked and untracked file of the open workspace.
    pub(super) fn open_any_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.active_ws() else {
            return notify_err(window, cx, "Open a workspace first");
        };
        let repos: Vec<_> = (ws.repos.iter())
            .map(|r| (r.name.clone(), ws.dir().join(&r.name)))
            .collect();
        let shark = cx.entity().downgrade();
        let picker = cx.new(|cx| FilePicker::new(repos, shark, window, cx));
        let state = picker.read(cx).state.clone();
        window.open_dialog(cx, move |d, _, cx| menu_dialog(d, cx).child(picker.clone()));
        state.update(cx, |p, cx| p.focus(window, cx));
    }

    /// Opens a picked path; text files only, since the editor would show a binary one empty.
    fn open_any_path(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.active_ws() else { return };
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
        self.open_path(path, window, cx);
    }
}
