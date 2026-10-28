//! Location-aware navigation and atomic preflight for language-server edits.
use super::*;
use crate::editor::LanguageEvent;
use crate::lsp::{
    Session, SessionEvent, client,
    document::{self, Encoding, Snapshot},
    tools,
};
use lsp_types::{LocationLink, Uri, WorkspaceEdit};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone)]
struct Visit {
    path: PathBuf,
    selection: std::ops::Range<usize>,
    scroll: Point<Pixels>,
}

#[derive(Default)]
pub(super) struct History {
    back: Vec<Visit>,
    forward: Vec<Visit>,
}

struct Staged {
    path: PathBuf,
    relative: String,
    before: String,
    after: String,
    version: Option<i32>,
}

impl Shark {
    pub(super) fn choose_or_restart_language(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = self
            .editor
            .as_ref()
            .and_then(|e| e.read(cx).intelligence.as_ref().map(|l| l.session.clone()));
        let Some(session) = session else {
            return self.restart_language(window, cx);
        };
        let choices = session.read(cx).choices.clone();
        if choices.is_empty() {
            return self.restart_language(window, cx);
        }
        let items = choices
            .iter()
            .map(|path| CommandItem::new().label(path.display().to_string()))
            .collect();
        open_menu(
            cx.entity().downgrade(),
            "Choose Xcode project…",
            items,
            0,
            move |_, ix, window, cx| {
                let project = session.read(cx).project.clone();
                let path = choices[ix].clone();
                let session = session.downgrade();
                cx.spawn_in(window, async move |_, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move { tools::select_xcode(&project, &path) })
                        .await;
                    let _ = session.update_in(cx, |s, window, cx| match result {
                        Ok(()) => s.restart(window, cx),
                        Err(error) => {
                            s.detail = Some(error.to_string());
                            cx.notify();
                        }
                    });
                })
                .detach();
            },
            window,
            cx,
        );
    }

    pub(super) fn restart_language(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.clone() else {
            return;
        };
        let session = editor
            .read(cx)
            .intelligence
            .as_ref()
            .map(|l| l.session.clone());
        match session {
            Some(session) => session.update(cx, |s, cx| s.restart(window, cx)),
            None => self.start_language(&editor, window, cx),
        }
    }

    pub(super) fn rename_symbol(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.clone() else {
            return;
        };
        if editor.read(cx).external {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("New symbol name"));
        let weak = cx.entity().downgrade();
        window.open_dialog(cx, move |d, _, _| {
            let input = input.clone();
            let weak = weak.clone();
            let editor = editor.clone();
            d.title("Rename symbol")
                .child(Input::new(&input))
                .on_ok(move |_, window, cx| {
                    let name = input.read(cx).value().to_string();
                    if name.trim().is_empty() {
                        return false;
                    }
                    let _ = weak.update(cx, |s, cx| {
                        if s.editor.as_ref() == Some(&editor) {
                            s.language_action("textDocument/rename", Some(name), window, cx);
                        }
                    });
                    true
                })
        });
    }

    pub(super) fn language_action(
        &mut self,
        method: &'static str,
        new_name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.editor.clone() else {
            return;
        };
        let e = editor.read(cx);
        let Some(language) = &e.intelligence else {
            return notify_err(window, cx, "Language tools are not ready");
        };
        let Some(client) = (if method == "textDocument/formatting" {
            language
                .companion
                .clone()
                .or_else(|| language.client.clone())
        } else {
            language.client.clone()
        }) else {
            return notify_err(window, cx, "Language tools are not ready");
        };
        let uri = language.uri.clone();
        let snapshots = language.snapshot.clone();
        let root = language.session.read(cx).project.worktree.clone();
        let text = e.input.read(cx).value().to_string();
        let cursor = e.input.read(cx).cursor();
        let workspace = self.active.clone();
        let guard = document::EditGuard::capture();
        let request_editor = editor.clone();
        let editor = editor.downgrade();
        let task = cx.spawn_in(window, async move |this, cx| {
            let response = async {
                let mut snapshot = snapshots.lock().unwrap().clone();
                for _ in 0..100 {
                    if snapshot.text.as_ref() == text { break; }
                    cx.background_executor().timer(std::time::Duration::from_millis(10)).await;
                    snapshot = snapshots.lock().unwrap().clone();
                }
                anyhow::ensure!(snapshot.text.as_ref() == text, "Document synchronization is pending; try again");
                let encoding = Encoding::from(client.encoding());
                let position = document::position(&text, cursor, encoding)?;
                let mut params = json!({"textDocument":{"uri":uri}, "position":position});
                match method {
                    "textDocument/formatting" => { params = json!({"textDocument":{"uri":uri},"options":{"tabSize":4,"insertSpaces":true}}); }
                    "textDocument/references" => { params["context"] = json!({"includeDeclaration":true}); }
                    "textDocument/rename" => { params["newName"] = json!(new_name); }
                    _ => {}
                }
                if method == "textDocument/rename" && client.capabilities().and_then(|c| c.rename_provider).is_some_and(|p| matches!(p, lsp_types::OneOf::Right(o) if o.prepare_provider == Some(true))) {
                    let prepared = client.request("textDocument/prepareRename", json!({"textDocument":{"uri":uri},"position":position})).await?;
                    anyhow::ensure!(!prepared.is_null(), "This symbol cannot be renamed");
                }
                let result = client.request(method, params).await?;
                anyhow::ensure!(*snapshots.lock().unwrap() == snapshot, "The document changed; request the result again");
                Ok::<_, anyhow::Error>((result, encoding))
            }.await;
            let _ = this.update_in(cx, |s, window, cx| {
                let Some(editor) = editor.upgrade() else { return };
                if s.active != workspace || s.editor.as_ref() != Some(&editor) || editor.read(cx).input.read(cx).cursor() != cursor || editor.read(cx).input.read(cx).value().as_ref() != text { return; }
                let (result, encoding) = match response { Ok(r) => r, Err(error) => return notify_err(window, cx, error.to_string()) };
                if result.is_null() { return window.push_notification("No result at this position", cx); }
                match method {
                    "textDocument/formatting" => {
                        match serde_json::from_value::<Vec<lsp_types::TextEdit>>(result) {
                            Ok(edits) => s.apply_workspace_edit(WorkspaceEdit { changes: Some(std::collections::HashMap::from([(uri, edits)])), ..Default::default() }, encoding, root, Some(guard), None, window, cx),
                            Err(error) => notify_err(window, cx, error.to_string()),
                        }
                    }
                    "textDocument/rename" => match serde_json::from_value(result) {
                        Ok(edit) => s.apply_workspace_edit(edit, encoding, root, Some(guard), None, window, cx),
                        Err(error) => notify_err(window, cx, error.to_string()),
                    },
                    "textDocument/references" => match serde_json::from_value::<Vec<lsp_types::Location>>(result) {
                        Ok(locations) => s.pick_locations(locations.into_iter().map(crate::lsp::providers::location_link).collect(), encoding, window, cx),
                        Err(error) => notify_err(window, cx, error.to_string()),
                    },
                    "textDocument/documentSymbol" => {
                        let mut symbols = vec![];
                        collect_symbols(&result, &uri, &mut symbols);
                        let items = symbols.iter().map(|(label, _)| CommandItem::new().label(label.clone())).collect();
                        open_menu(cx.entity().downgrade(), "Go to symbol…", items, 0, move |s, ix, w, cx| {
                            if let Some((_, location)) = symbols.get(ix) { s.open_location(location.clone(), encoding, true, w, cx); }
                        }, window, cx);
                    }
                    "textDocument/signatureHelp" => {
                        let label = result["signatures"].as_array().map(|signatures| signatures.iter().filter_map(|s| s["label"].as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default();
                        if !label.is_empty() { window.push_notification(label, cx); }
                    }
                    _ => {}
                }
            });
        });
        request_editor.update(cx, |e, _| e.lsp_request = Some(task));
    }

    pub(super) fn start_language(
        &mut self,
        editor: &Entity<FileEditor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // UI fixtures attach a controllable session explicitly; never install tools during unit tests.
        if cfg!(test) {
            return;
        }
        let Some(ws) = self.active_ws() else { return };
        let path = editor.read(cx).path.clone();
        let Some(repo) = path
            .split('/')
            .next()
            .filter(|name| ws.repos.iter().any(|r| r.name == *name))
        else {
            return;
        };
        let root = ws.dir().join(repo);
        let file = ws.dir().join(&path);
        let weak = editor.downgrade();
        let workspace = self.active.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let project = cx
                .background_executor()
                .spawn(async move { tools::discover(&file, &root) })
                .await;
            let _ = this.update_in(cx, |s, window, cx| {
                if s.active != workspace {
                    return;
                }
                let Some(editor) = weak.upgrade() else { return };
                match project {
                    Ok(Some(project)) => {
                        let key = project.session_key();
                        s.language_sessions
                            .retain(|_, (session, _)| session.upgrade().is_some());
                        let session =
                            match s.language_sessions.get(&key).and_then(|(e, _)| e.upgrade()) {
                                Some(session) => session,
                                None => {
                                    let session = cx.new(|cx| Session::new(project, window, cx));
                                    let sub = cx.subscribe_in(
                                        &session,
                                        window,
                                        |s, session, event: &SessionEvent, w, cx| {
                                            if let SessionEvent::Request(request) = event
                                                && let Some(request) = request.borrow_mut().take()
                                            {
                                                s.language_request(session, request, w, cx);
                                            }
                                        },
                                    );
                                    s.language_sessions.insert(key, (session.downgrade(), sub));
                                    session
                                }
                            };
                        editor.update(cx, |e, cx| e.attach_language(session, window, cx));
                    }
                    Ok(None) => {}
                    Err(error) => editor.update(cx, |e, cx| {
                        e.lsp_error = Some(format!("{error:#}"));
                        cx.notify();
                    }),
                }
                cx.notify();
            });
        });
        editor.update(cx, |e, _| e.lsp_setup = Some(task));
    }

    pub(super) fn language_event(
        &mut self,
        editor: &Entity<FileEditor>,
        event: &LanguageEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            LanguageEvent::Navigate(locations, encoding) => {
                self.pick_locations(locations.clone(), *encoding, window, cx)
            }
            LanguageEvent::Apply {
                edit,
                snapshot,
                encoding,
                guard,
            } => {
                if editor.read(cx).input.read(cx).value().as_ref() != snapshot.text.as_ref() {
                    return notify_err(window, cx, "The document changed; request the edit again");
                }
                let Some(language) = &editor.read(cx).intelligence else {
                    return;
                };
                let root = language.session.read(cx).project.worktree.clone();
                self.apply_workspace_edit(
                    edit.clone(),
                    *encoding,
                    root,
                    Some(guard.clone()),
                    None,
                    window,
                    cx,
                );
            }
        }
    }

    fn language_request(
        &mut self,
        session: &Entity<Session>,
        event: client::Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            client::Event::ApplyEdit { params, reply } => {
                match serde_json::from_value::<WorkspaceEdit>(params["edit"].clone()) {
                    Ok(edit) => {
                        let session = session.read(cx);
                        let Some(client) = &session.client else {
                            return reply.reject("Session stopped");
                        };
                        self.apply_workspace_edit(
                            edit,
                            client.encoding().into(),
                            session.project.worktree.clone(),
                            None,
                            Some(reply),
                            window,
                            cx,
                        );
                    }
                    Err(error) => {
                        reply.respond(json!({"applied":false,"failureReason":error.to_string()}))
                    }
                }
            }
            client::Event::ShowDocument { params, reply } => {
                let parsed = serde_json::from_value::<lsp_types::ShowDocumentParams>(params);
                match parsed {
                    Ok(p) if p.uri.scheme().is_some_and(|s| s.as_str() == "file") => {
                        let range = p.selection.unwrap_or_default();
                        let encoding = session
                            .read(cx)
                            .client
                            .as_ref()
                            .map(|c| c.encoding().into())
                            .unwrap_or(Encoding::Utf16);
                        self.open_location(
                            LocationLink {
                                origin_selection_range: None,
                                target_uri: p.uri,
                                target_range: range,
                                target_selection_range: range,
                            },
                            encoding,
                            true,
                            window,
                            cx,
                        );
                        reply.respond(json!({"success":true}));
                    }
                    _ => reply.respond(json!({"success":false})),
                }
            }
            client::Event::RegisterCapability { params, reply } => {
                match session
                    .read(cx)
                    .watcher
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("File watcher unavailable"))
                    .and_then(|w| w.register(&params))
                {
                    Ok(ready) => {
                        cx.spawn(async move |_, _| match ready.recv().await {
                            Ok(Ok(())) => reply.respond(Value::Null),
                            Ok(Err(error)) => reply.reject(error),
                            Err(_) => reply.reject("File watcher stopped before registration"),
                        })
                        .detach();
                    }
                    Err(error) => reply.reject(error.to_string()),
                }
            }
            client::Event::UnregisterCapability { params, reply } => {
                match session
                    .read(cx)
                    .watcher
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("File watcher unavailable"))
                    .and_then(|w| w.unregister(&params))
                {
                    Ok(()) => reply.respond(Value::Null),
                    Err(error) => reply.reject(error.to_string()),
                }
            }
            _ => {}
        }
    }

    pub(super) fn pick_locations(
        &mut self,
        locations: Vec<LocationLink>,
        encoding: Encoding,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if locations.is_empty() {
            return window.push_notification("No locations found", cx);
        }
        if locations.len() == 1 {
            return self.open_location(locations[0].clone(), encoding, true, window, cx);
        }
        let items = locations
            .iter()
            .map(|l| {
                CommandItem::new().label(format!(
                    "{}:{}:{}",
                    uri_path(&l.target_uri)
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|_| l.target_uri.to_string()),
                    l.target_selection_range.start.line + 1,
                    l.target_selection_range.start.character + 1
                ))
            })
            .collect();
        open_menu(
            cx.entity().downgrade(),
            "Choose location…",
            items,
            0,
            move |s, ix, w, cx| {
                s.open_location(locations[ix].clone(), encoding, true, w, cx);
            },
            window,
            cx,
        );
    }

    fn visit(&self, cx: &App) -> Option<Visit> {
        let editor = self.editor.as_ref()?.read(cx);
        Some(Visit {
            path: editor.absolute_path().to_owned(),
            selection: editor.input.read(cx).selected_range(),
            scroll: editor.input.read(cx).scroll_offset(),
        })
    }

    pub(super) fn navigate_history(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.visit(cx);
        let target = if forward {
            self.navigation.forward.pop()
        } else {
            self.navigation.back.pop()
        };
        let Some(target) = target else { return };
        if let Some(current) = current {
            if forward {
                self.navigation.back.push(current);
            } else {
                self.navigation.forward.push(current);
            }
        }
        self.open_source(target.path.clone(), None, Some(target), false, window, cx);
    }

    pub(super) fn open_location(
        &mut self,
        location: LocationLink,
        encoding: Encoding,
        history: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match uri_path(&location.target_uri) {
            Ok(path) => self.open_source(
                path,
                Some((location.target_selection_range, encoding)),
                None,
                history,
                window,
                cx,
            ),
            Err(error) => notify_err(window, cx, error.to_string()),
        }
    }

    fn open_source(
        &mut self,
        path: PathBuf,
        range: Option<(lsp_types::Range, Encoding)>,
        visit: Option<Visit>,
        history: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.active_ws() else { return };
        let workspace = self.active.clone();
        let directory = ws.dir();
        let roots: Vec<_> = ws.repos.iter().map(|r| directory.join(&r.name)).collect();
        let old = history.then(|| self.visit(cx)).flatten();
        let open_paths: Vec<_> = self
            .file_tabs
            .iter()
            .map(|tab| tab.editor.read(cx).absolute_path().to_owned())
            .collect();
        cx.spawn_in(window, async move |this, cx| {
            let read = cx
                .background_executor()
                .spawn(async move {
                    let canonical = path.canonicalize()?;
                    let text = read_source(&canonical)?;
                    let internal = roots
                        .iter()
                        .any(|r| r.canonicalize().is_ok_and(|r| canonical.starts_with(r)));
                    let relative = if internal {
                        directory.canonicalize().ok().and_then(|d| {
                            canonical
                                .strip_prefix(d)
                                .ok()
                                .map(|p| p.to_string_lossy().to_string())
                        })
                    } else {
                        None
                    };
                    let existing = open_paths
                        .into_iter()
                        .find(|p| p == &path || p.canonicalize().is_ok_and(|p| p == canonical));
                    Ok::<_, anyhow::Error>((canonical, path, text, relative, existing))
                })
                .await;
            let _ = this.update_in(cx, |s, window, cx| {
                if s.active != workspace {
                    return;
                }
                let (canonical, original, text, relative, existing_path) = match read {
                    Ok(read) => read,
                    Err(error) => return notify_err(window, cx, error.to_string()),
                };
                let existing = s.file_tabs.iter().position(|tab| {
                    existing_path
                        .as_ref()
                        .is_some_and(|p| tab.editor.read(cx).absolute_path() == p)
                        || tab.editor.read(cx).absolute_path() == original
                        || relative
                            .as_ref()
                            .is_some_and(|p| tab.editor.read(cx).path == *p)
                });
                if let Some(relative) = &relative
                    && s.differ
                        .read(cx)
                        .edit_snapshot()
                        .is_some_and(|e| e.path.as_str() == relative && (e.dirty || e.saving))
                {
                    return notify_err(window, cx, "The target has unsaved edits in the diff");
                }
                let source = existing
                    .map(|ix| {
                        s.file_tabs[ix]
                            .editor
                            .read(cx)
                            .input
                            .read(cx)
                            .value()
                            .to_string()
                    })
                    .unwrap_or(text.clone());
                let selected = match range {
                    Some((range, encoding)) => match (
                        document::offset(&source, range.start, encoding),
                        document::offset(&source, range.end, encoding),
                    ) {
                        (Ok(a), Ok(b)) if a <= b => a..b,
                        _ => {
                            return notify_err(
                                window,
                                cx,
                                "The target location no longer matches its document",
                            );
                        }
                    },
                    None => visit
                        .as_ref()
                        .map(|v| {
                            source.floor_char_boundary(v.selection.start.min(source.len()))
                                ..source.floor_char_boundary(v.selection.end.min(source.len()))
                        })
                        .unwrap_or(0..0),
                };
                let editor = match existing {
                    Some(ix) => s.file_tabs[ix].editor.clone(),
                    None => {
                        let external = relative.is_none();
                        let path = relative.unwrap_or_else(|| canonical.display().to_string());
                        let tab = s.make_file_tab(path.clone(), original, Some(text), window, cx);
                        tab.editor.update(cx, |e, cx| e.set_external(external, cx));
                        let editor = tab.editor.clone();
                        if !external {
                            s.active_ws_mut().unwrap().open_tab(TabId::File(path));
                        }
                        s.file_tabs.push(tab);
                        editor
                    }
                };
                if let Some(old) = old {
                    s.navigation.back.push(old);
                    s.navigation.forward.clear();
                }
                s.editor = Some(editor.clone());
                s.show(Tab::File, window, cx);
                editor.update(cx, |e, cx| {
                    e.input.update(cx, |i, cx| {
                        i.set_selected_range(selected, cx);
                        if let Some(visit) = visit {
                            i.set_scroll_offset(visit.scroll, cx);
                        }
                        i.focus(window, cx);
                    })
                });
                let _ = s.state.save();
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn apply_workspace_edit(
        &mut self,
        edit: WorkspaceEdit,
        encoding: Encoding,
        root: PathBuf,
        guard: Option<document::EditGuard>,
        reply: Option<client::Reply>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.active_ws() else { return };
        let workspace = self.active.clone();
        let directory = ws.dir();
        let inline = self.differ.read(cx).edit_snapshot();
        let open: Vec<_> = self
            .file_tabs
            .iter()
            .map(|tab| {
                let e = tab.editor.read(cx);
                let text = e.input.read(cx).value().to_string();
                let version = e.intelligence.as_ref().and_then(|l| {
                    let s = l.snapshot.lock().unwrap();
                    (s.text.as_ref() == text).then_some(s.version)
                });
                (
                    e.absolute_path().to_owned(),
                    text,
                    version,
                    e.conflict || e.saving || e.external,
                    e.disk_fingerprint(),
                )
            })
            .collect();
        cx.spawn_in(window, async move |this, cx| {
            let preflight_guard = guard.clone();
            let staged = cx
                .background_executor()
                .spawn(async move {
                    anyhow::ensure!(
                        preflight_guard.as_ref().is_none_or(|g| g.is_current()),
                        "Documents changed after the language request; try again"
                    );
                    let root = root.canonicalize()?;
                    let directory = directory.canonicalize()?;
                    let mut seen = HashSet::new();
                    document::workspace_edits(edit)?
                        .into_iter()
                        .map(|(uri, expected, edits)| {
                            let path = uri_path(&uri)?.canonicalize()?;
                            anyhow::ensure!(
                                path.starts_with(&root),
                                "Edit leaves the current worktree: {}",
                                path.display()
                            );
                            anyhow::ensure!(
                                seen.insert(path.clone()),
                                "Duplicate document in workspace edit"
                            );
                            let relative =
                                path.strip_prefix(&directory)?.to_string_lossy().to_string();
                            let aliases: Vec<_> = open
                                .iter()
                                .filter(|(p, _, _, _, _)| p.canonicalize().is_ok_and(|p| p == path))
                                .collect();
                            anyhow::ensure!(
                                aliases.len() <= 1,
                                "Close duplicate aliases of {} before editing",
                                path.display()
                            );
                            let existing = aliases.first().copied();
                            anyhow::ensure!(
                                preflight_guard.is_some() || expected.is_some(),
                                "Unversioned server edits require a fresh editor request"
                            );
                            let disk = read_source(&path)?;
                            if let Some(guard) = &preflight_guard {
                                ensure_disk_before_request(&path, guard.started)?;
                            }
                            let (before, version) = match existing {
                                Some((_, text, version, blocked, fingerprint)) => {
                                    anyhow::ensure!(
                                        !blocked,
                                        "Resolve the save or disk conflict in {relative} first"
                                    );
                                    anyhow::ensure!(
                                        crate::files::fingerprint(disk.as_bytes()) == *fingerprint,
                                        "{relative} changed on disk; reload before applying edits"
                                    );
                                    (text.clone(), *version)
                                }
                                None => (disk, None),
                            };
                            anyhow::ensure!(
                                expected.is_none() || expected == version,
                                "Stale version for {relative}"
                            );
                            let snapshot = Snapshot {
                                text: Arc::from(before.as_str()),
                                version: version.unwrap_or(0),
                            };
                            let after =
                                document::apply_edits(&snapshot, expected, &edits, encoding)?;
                            anyhow::ensure!(
                                after.len() as u64 <= crate::editor::MAX_BYTES,
                                "Edit exceeds the 4 MiB editor limit"
                            );
                            Ok(Staged {
                                path,
                                relative,
                                before,
                                after,
                                version,
                            })
                        })
                        .collect::<anyhow::Result<Vec<_>>>()
                })
                .await;
            let _ = this.update_in(cx, |s, window, cx| {
                let result = (|| -> anyhow::Result<()> {
                    anyhow::ensure!(
                        s.active == workspace && s.differ.read(cx).edit_snapshot() == inline,
                        "Workspace or diff changed; request the edit again"
                    );
                    anyhow::ensure!(
                        guard.as_ref().is_none_or(|g| g.is_current()),
                        "Documents changed after the language request; try again"
                    );
                    let staged = staged?;
                    for edit in &staged {
                        anyhow::ensure!(
                            !inline.as_ref().is_some_and(
                                |e| e.path.as_str() == edit.relative && (e.dirty || e.saving)
                            ),
                            "{} has unsaved diff edits",
                            edit.relative
                        );
                        let existing = s.file_tabs.iter().find(|tab| {
                            tab.editor.read(cx).absolute_path() == edit.path
                                || tab.editor.read(cx).path == edit.relative
                        });
                        if let Some(tab) = existing {
                            let e = tab.editor.read(cx);
                            anyhow::ensure!(
                                !e.conflict
                                    && !e.saving
                                    && !e.external
                                    && e.input.read(cx).value().as_ref() == edit.before
                                    && e.intelligence.as_ref().map(|l| l
                                        .snapshot
                                        .lock()
                                        .unwrap()
                                        .version)
                                        == edit.version,
                                "{} changed; request the edit again",
                                edit.relative
                            );
                        } else {
                            anyhow::ensure!(
                                edit.version.is_none(),
                                "A target editor closed; request the edit again"
                            );
                        }
                    }
                    for edit in staged {
                        if edit.before == edit.after {
                            continue;
                        }
                        let editor = match s.file_tabs.iter().find(|tab| {
                            tab.editor.read(cx).absolute_path() == edit.path
                                || tab.editor.read(cx).path == edit.relative
                        }) {
                            Some(tab) => tab.editor.clone(),
                            None => {
                                let tab = s.make_file_tab(
                                    edit.relative.clone(),
                                    edit.path,
                                    Some(edit.before),
                                    window,
                                    cx,
                                );
                                let editor = tab.editor.clone();
                                s.active_ws_mut()
                                    .unwrap()
                                    .open_tab(TabId::File(edit.relative));
                                s.file_tabs.push(tab);
                                editor
                            }
                        };
                        editor.update(cx, |e, cx| e.replace_language_text(edit.after, window, cx));
                    }
                    let _ = s.state.save();
                    cx.notify();
                    Ok(())
                })();
                if let Some(reply) = reply {
                    reply.respond(match &result {
                        Ok(()) => json!({"applied":true}),
                        Err(error) => json!({"applied":false,"failureReason":error.to_string()}),
                    });
                }
                if let Err(error) = result {
                    notify_err(window, cx, error.to_string());
                }
            });
        })
        .detach();
    }
}

fn uri_path(uri: &Uri) -> anyhow::Result<PathBuf> {
    url::Url::parse(uri.as_str())?
        .to_file_path()
        .map_err(|_| anyhow::anyhow!("Only local file locations are supported"))
}

fn collect_symbols(value: &Value, uri: &Uri, out: &mut Vec<(String, LocationLink)>) {
    let Some(symbols) = value.as_array() else {
        return;
    };
    for symbol in symbols {
        let location = if let Ok(location) =
            serde_json::from_value::<lsp_types::Location>(symbol["location"].clone())
        {
            Some(crate::lsp::providers::location_link(location))
        } else {
            serde_json::from_value::<lsp_types::Range>(symbol["selectionRange"].clone())
                .ok()
                .map(|range| LocationLink {
                    origin_selection_range: None,
                    target_uri: uri.clone(),
                    target_range: range,
                    target_selection_range: range,
                })
        };
        if let Some(location) = location {
            out.push((
                symbol["name"].as_str().unwrap_or("Symbol").to_string(),
                location,
            ));
        }
        collect_symbols(&symbol["children"], uri, out);
    }
}

fn read_source(path: &Path) -> anyhow::Result<String> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    anyhow::ensure!(file.metadata()?.is_file(), "Not a regular source file");
    let mut bytes = Vec::new();
    file.take(crate::editor::MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= crate::editor::MAX_BYTES && !bytes.contains(&0),
        "Binary file or larger than 4 MiB"
    );
    Ok(String::from_utf8(bytes)?)
}

fn ensure_disk_before_request(path: &Path, started: std::time::SystemTime) -> anyhow::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let metadata = path.metadata()?;
    let changed = std::time::UNIX_EPOCH
        + std::time::Duration::new(
            metadata.ctime().try_into()?,
            metadata.ctime_nsec().try_into()?,
        );
    anyhow::ensure!(
        metadata.modified()? <= started && changed <= started,
        "{} changed on disk after the language request; try again",
        path.display()
    );
    Ok(())
}
