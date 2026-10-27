//! File-level discard uses the same action from tree rows, editor headers and file tabs.
use super::*;

impl Shark {
    fn can_discard_file(&self, name: &str, path: &str, cx: &App) -> Result<Workspace, String> {
        let ws = self
            .active_ws()
            .filter(|ws| ws.name == name)
            .ok_or("The workspace changed; open the file menu again")?;
        if self.archiving.contains(name) {
            return Err("This workspace is being archived".into());
        }
        if let Some(reason) = self
            .syncs
            .get(name)
            .and_then(|s| s.read(cx).blocked(true, cx))
        {
            return Err(reason);
        }
        if self.file_tabs.iter().any(|t| {
            let e = t.editor.read(cx);
            e.path == path && e.saving
        }) || self
            .editing
            .as_ref()
            .is_some_and(|e| e.path == path && e.saving)
        {
            return Err(format!("Saving {path}; wait before discarding"));
        }
        Ok(ws.clone())
    }

    pub(super) fn discard_uncommitted(
        &mut self,
        name: &str,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(e) = self.can_discard_file(name, path, cx) {
            return notify_err(window, cx, e);
        }
        let (name, path) = (name.to_string(), path.to_string());
        confirm(
            format!("Discard uncommitted changes to {path}?"),
            "Restores this file to its last commit and discards staged changes and unsaved editor edits. New files are deleted. This cannot be undone.",
            "Discard",
            move |s, window, cx| {
                let ws = match s.can_discard_file(&name, &path, cx) {
                    Ok(ws) => ws,
                    Err(e) => return notify_err(window, cx, e),
                };
                let Some((repo, rel)) = path.split_once('/') else {
                    return;
                };
                if !ws.repos.iter().any(|r| r.name == repo) {
                    return notify_err(window, cx, "This file is not in a workspace repository");
                }
                // shortcut: restore blocks app saves; add editor write locks and a worker if Git filters make it slow.
                if let Err(e) = git::discard_file(&ws.dir().join(repo), rel) {
                    s.refresh_diff(cx);
                    return notify_err(window, cx, e);
                }
                if s.editing.as_ref().is_some_and(|e| e.path == path) {
                    s.end_edit(cx);
                    if s.tab == Tab::Diff {
                        s.diff.read(cx).focus_handle(cx).focus(window, cx);
                    }
                }
                let editors: Vec<_> = s
                    .file_tabs
                    .iter()
                    .filter(|t| t.editor.read(cx).path == path)
                    .map(|t| t.editor.clone())
                    .collect();
                for editor in editors {
                    if std::fs::symlink_metadata(ws.dir().join(&path)).is_ok() {
                        editor.update(cx, |e, cx| e.discard(window, cx));
                    } else {
                        editor.update(cx, |e, _| e.dirty = false);
                        s.close_editor(editor, window, cx);
                    }
                }
                s.uncommitted.remove(&path);
                s.refresh_diff(cx);
                cx.notify();
            },
            window,
            cx,
        );
    }
}
