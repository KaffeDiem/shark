//! Location-aware navigation and atomic preflight for language-server edits.
use super::*;
use crate::editor::LanguageEvent;
use crate::lsp::{
    Session, SessionEvent, client,
    document::{self, Encoding, Snapshot},
    tools,
};
use gpui_kit::base::input::RopeExt as _;
use lsp_types::{LocationLink, Uri, WorkspaceEdit};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone)]
pub(super) struct Visit {
    pub(super) path: PathBuf,
    selection: std::ops::Range<usize>,
    scroll: Point<Pixels>,
    /// The editor's session: an external source has none of its own to rediscover.
    session: Option<WeakEntity<Session>>,
}

#[derive(Default)]
pub(super) struct History {
    pub(super) back: Vec<Visit>,
    pub(super) forward: Vec<Visit>,
    request: u64,
    /// The Back/Forward still loading: its direction, the entries it steps over, its request.
    pending: Option<(bool, usize, u64)>,
}

impl History {
    fn steps(&self, request: u64) -> usize {
        self.pending.filter(|p| p.2 == request).map_or(1, |p| p.1)
    }
}

pub(super) enum LanguageAction {
    Format,
    Rename(String),
    References,
    SignatureHelp,
}

struct Staged {
    path: PathBuf,
    existing_path: Option<PathBuf>,
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
        let editors = if let Some(session) = session {
            self.language_sessions
                .retain(|_, (cached, _)| *cached != session);
            self.file_tabs
                .iter()
                .filter(|tab| {
                    tab.editor
                        .read(cx)
                        .intelligence
                        .as_ref()
                        .is_some_and(|language| language.session == session)
                })
                .map(|tab| tab.editor.clone())
                .collect::<Vec<_>>()
        } else {
            vec![editor]
        };
        // Retry discovers current environments and regroups every document that shared the server.
        for editor in editors {
            editor.update(cx, |e, cx| {
                e.intelligence = None;
                e.lsp_setup = None;
                e.lsp_request = None;
                e.lsp_error = None;
                e.outline.clear();
                e.crumbs.clear();
                e.input.update(cx, |input, cx| {
                    *input.lsp_mut() = Default::default();
                    if let Some(diagnostics) = input.diagnostics_mut() {
                        diagnostics.clear();
                    }
                    cx.notify();
                });
                cx.notify();
            });
            self.start_language(&editor, window, cx);
        }
    }

    pub(super) fn rename_symbol(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.clone() else {
            return;
        };
        if editor.read(cx).external {
            return;
        }
        let word = {
            let i = editor.read(cx).input.read(cx);
            (i.text().word_range(i.cursor())).map(|r| i.text().slice(r).to_string())
        };
        let input = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("New symbol name");
            if let Some(word) = word {
                input.set_value(word, window, cx);
                input.select_all(window, cx);
            }
            input
        });
        let weak = cx.entity().downgrade();
        let field = input.clone();
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
                            s.language_action(LanguageAction::Rename(name), window, cx);
                        }
                    });
                    true
                })
        });
        field.update(cx, |i, cx| i.focus(window, cx));
    }

    pub(super) fn language_action(
        &mut self,
        action: LanguageAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use LanguageAction::*;
        let Some(editor) = self.editor.clone() else {
            return;
        };
        let e = editor.read(cx);
        let Some(language) = &e.intelligence else {
            return notify_err(window, cx, "Language tools are not ready");
        };
        let Some(client) = (match action {
            Format => (language.companion.clone()).or_else(|| language.client.clone()),
            _ => language.client.clone(),
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
                let at = json!({"textDocument":{"uri":uri}, "position":position});
                let (method, params) = match &action {
                    Format => ("textDocument/formatting", json!({"textDocument":{"uri":uri},"options":formatting_options(&text)})),
                    References => ("textDocument/references", json!({"textDocument":{"uri":uri}, "position":position, "context":{"includeDeclaration":true}})),
                    Rename(name) => ("textDocument/rename", json!({"textDocument":{"uri":uri}, "position":position, "newName":name})),
                    SignatureHelp => ("textDocument/signatureHelp", at.clone()),
                };
                if matches!(action, Rename(_)) && client.capabilities().and_then(|c| c.rename_provider).is_some_and(|p| matches!(p, lsp_types::OneOf::Right(o) if o.prepare_provider == Some(true))) {
                    let prepared = client.request("textDocument/prepareRename", at).await?;
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
                match action {
                    Format => {
                        match serde_json::from_value::<Vec<lsp_types::TextEdit>>(result) {
                            Ok(edits) => s.apply_workspace_edit(WorkspaceEdit { changes: Some(std::collections::HashMap::from([(uri, edits)])), ..Default::default() }, encoding, root, Some(guard), None, window, cx),
                            Err(error) => notify_err(window, cx, error.to_string()),
                        }
                    }
                    Rename(_) => match serde_json::from_value(result) {
                        Ok(edit) => s.apply_workspace_edit(edit, encoding, root, Some(guard), None, window, cx),
                        Err(error) => notify_err(window, cx, error.to_string()),
                    },
                    References => match serde_json::from_value::<Vec<lsp_types::Location>>(result) {
                        Ok(locations) => s.pick_locations(locations.into_iter().map(crate::lsp::providers::location_link).collect(), encoding, false, window, cx),
                        Err(error) => notify_err(window, cx, error.to_string()),
                    },
                    SignatureHelp => {
                        let label = result["signatures"].as_array().map(|signatures| signatures.iter().filter_map(|s| s["label"].as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default();
                        if !label.is_empty() { window.push_notification(label, cx); }
                    }
                }
            });
        });
        request_editor.update(cx, |e, _| e.lsp_request = Some(task));
    }

    /// The keyboard Go to definition: the editor's own Command-click request at the cursor.
    pub(super) fn go_to_definition(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.clone() else {
            return;
        };
        editor.update(cx, |e, cx| e.input.update(cx, |i, cx| i.focus(window, cx)));
        window.dispatch_action(Box::new(gpui_kit::base::input::GoToDefinition), cx);
    }

    pub(super) fn start_language(
        &mut self,
        editor: &Entity<FileEditor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
            // Unit tests never install tools: discovery finds the fake-server session a test
            // registered for the file's worktree.
            let project = if cfg!(test) {
                let Ok(project) = this.read_with(cx, |s, cx| {
                    (s.language_sessions.values())
                        .map(|(session, _)| session.read(cx).project.clone())
                        .find(|p| file.starts_with(&p.worktree))
                }) else {
                    return;
                };
                Ok(project)
            } else {
                cx.background_executor()
                    .spawn(async move { tools::discover(&file, &root) })
                    .await
            };
            let _ = this.update_in(cx, |s, window, cx| {
                if s.active != workspace {
                    return;
                }
                let Some(editor) = weak.upgrade() else { return };
                match project {
                    Ok(Some(project)) => {
                        let session = match s.language_sessions.get(&project.session_key()) {
                            Some((session, _)) => session.clone(),
                            None => {
                                let session = cx.new(|cx| Session::new(project, window, cx));
                                s.register_session(session.clone(), window, cx);
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

    /// Keeps `session` for its project's editors, routing its server requests and messages.
    pub(super) fn register_session(
        &mut self,
        session: Entity<Session>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = session.read(cx).project.session_key();
        let sub = cx.subscribe_in(
            &session,
            window,
            |s, session, event: &SessionEvent, w, cx| match event {
                SessionEvent::Request(request) => {
                    if let Some(request) = request.borrow_mut().take() {
                        s.language_request(session, request, w, cx);
                    }
                }
                SessionEvent::Notification { method, params, .. }
                    if method == "window/showMessage" =>
                {
                    let message = params["message"].as_str().unwrap_or_default().to_string();
                    match params["type"].as_u64() {
                        Some(1) => notify_err(w, cx, message),
                        Some(2) => w.push_notification(Notification::warning(message), cx),
                        _ => {}
                    }
                }
                SessionEvent::Notification { .. } => {}
            },
        );
        self.language_sessions.insert(key, (session, sub));
    }

    /// Shuts down the servers no open file uses any more.
    pub(super) fn prune_language_sessions(&mut self, cx: &App) {
        let tabs = &self.file_tabs;
        self.language_sessions.retain(|_, (session, _)| {
            tabs.iter().any(|t| {
                let e = t.editor.read(cx);
                match &e.intelligence {
                    Some(l) => l.session == *session,
                    // A file still discovering (say, just restored) may be about to join it.
                    None => e
                        .absolute_path()
                        .starts_with(&session.read(cx).project.worktree),
                }
            })
        });
    }

    pub(super) fn language_event(
        &mut self,
        editor: &Entity<FileEditor>,
        event: &LanguageEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            LanguageEvent::Navigate(locations, encoding, new_tab) => {
                if self.editor.as_ref() != Some(editor) || self.tab != Tab::File {
                    return;
                }
                self.pick_locations(locations.clone(), *encoding, *new_tab, window, cx)
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
                match parsed.map(|p| (uri_path(&p.uri), p)) {
                    Ok((Ok(path), p)) => {
                        let encoding = session
                            .read(cx)
                            .client
                            .as_ref()
                            .map(|c| c.encoding().into())
                            .unwrap_or(Encoding::Utf16);
                        let range = p.selection.map(|range| (range, encoding));
                        // Without focus it opens in a background tab; success waits for the open.
                        let focus = p.take_focus != Some(false);
                        self.open_source(
                            path,
                            range,
                            None,
                            focus,
                            !focus,
                            Some((reply, focus, session.downgrade())),
                            window,
                            cx,
                        );
                    }
                    _ => reply.respond(json!({"success":false})),
                }
            }
            _ => {}
        }
    }

    pub(super) fn pick_locations(
        &mut self,
        locations: Vec<LocationLink>,
        encoding: Encoding,
        new_tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if locations.is_empty() {
            return window.push_notification("No locations found", cx);
        }
        if locations.len() == 1 {
            return self.open_location(locations[0].clone(), encoding, true, new_tab, window, cx);
        }
        let workspace = self.active.clone();
        let editor = self.editor.clone();
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
                if s.active != workspace || s.editor != editor {
                    return;
                }
                s.open_location(locations[ix].clone(), encoding, true, new_tab, w, cx);
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
            session: (editor.intelligence.as_ref()).map(|l| l.session.downgrade()),
        })
    }

    pub(super) fn navigate_history(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let nav = &self.navigation;
        // A press before the previous one landed steps past its target instead of repeating it.
        let steps = match nav.pending {
            Some((f, steps, request)) if f == forward && request == nav.request => steps + 1,
            _ => 1,
        };
        let from = if forward { &nav.forward } else { &nav.back };
        let Some(target) = (from.len().checked_sub(steps)).map(|ix| from[ix].clone()) else {
            return;
        };
        self.open_source(
            target.path.clone(),
            None,
            Some((target, forward)),
            false,
            false,
            None,
            window,
            cx,
        );
        self.navigation.pending = Some((forward, steps, self.navigation.request));
    }

    /// The outline of the open file as an indented tree; picking a symbol moves the cursor
    /// there. `selected` (an outline index) defaults to the innermost symbol at the cursor.
    pub(super) fn symbol_menu(
        &mut self,
        selected: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.editor.clone() else {
            return;
        };
        let e = editor.read(cx);
        if e.outline.is_empty() {
            let message = match e.intelligence {
                Some(_) => "No symbols in this file",
                None if e.external => "No language tools for this dependency source",
                None => "Language tools are not ready",
            };
            return window.push_notification(message, cx);
        }
        let items = (e.outline.iter())
            .map(|s| CommandItem::new().label(format!("{}{}", "    ".repeat(s.depth), s.name)))
            .collect();
        // The outline can refresh while the menu is open; a row keeps the symbol it showed.
        let targets: Vec<usize> = e.outline.iter().map(|s| s.target).collect();
        let selected = selected.or(e.crumbs.last().copied()).unwrap_or(0);
        open_menu(
            cx.entity().downgrade(),
            "Go to symbol…",
            items,
            selected,
            move |s, ix, w, cx| {
                if s.editor.as_ref() == Some(&editor) {
                    s.jump_to_symbol(targets[ix], w, cx);
                }
            },
            window,
            cx,
        );
    }

    fn jump_to_symbol(&mut self, target: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.clone() else {
            return;
        };
        self.navigation.request += 1;
        if let Some(old) = self.visit(cx) {
            self.navigation.back.push(old);
            self.navigation.forward.clear();
        }
        editor.update(cx, |e, cx| {
            e.input.update(cx, |i, cx| {
                // The outline may trail the newest edit by a sync.
                let at = i.value().floor_char_boundary(target.min(i.value().len()));
                i.set_selected_range(at..at, cx);
                i.focus(window, cx);
            })
        });
        cx.notify();
    }

    pub(super) fn open_location(
        &mut self,
        location: LocationLink,
        encoding: Encoding,
        history: bool,
        new_tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match uri_path(&location.target_uri) {
            Ok(path) => self.open_source(
                path,
                Some((location.target_selection_range, encoding)),
                None,
                history,
                new_tab,
                None,
                window,
                cx,
            ),
            Err(error) => notify_err(window, cx, error.to_string()),
        }
    }

    /// The open tab showing `path`: through an alias's open path, its own, or its workspace path.
    fn find_tab(
        &self,
        alias: Option<&Path>,
        path: &Path,
        relative: Option<&str>,
        cx: &App,
    ) -> Option<usize> {
        self.file_tabs.iter().position(|tab| {
            let e = tab.editor.read(cx);
            alias.is_some_and(|p| e.absolute_path() == p)
                || e.absolute_path() == path
                || relative.is_some_and(|p| e.path == p)
        })
    }

    /// Opens `path` at `range` or `visit`: in its open tab if it has one, else in place of
    /// the current file (unless `new_tab`, or the current file has unsaved edits). A server's
    /// `window/showDocument` passes its reply, answered once the file shows, whether to take
    /// focus, and its session, which an external file it opens attaches to.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn open_source(
        &mut self,
        path: PathBuf,
        range: Option<(lsp_types::Range, Encoding)>,
        visit: Option<(Visit, bool)>,
        history: bool,
        new_tab: bool,
        reply: Option<(client::Reply, bool, WeakEntity<Session>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.active_ws() else { return };
        let workspace = self.active.clone();
        let directory = ws.dir();
        let roots: Vec<_> = ws.repos.iter().map(|r| directory.join(&r.name)).collect();
        let old = (history || visit.is_some())
            .then(|| self.visit(cx))
            .flatten();
        let origin = self.editor.clone();
        let origin_tab = self.tab;
        // Dependency sources have no project of their own; they use the server that found them.
        let session = match (&visit, &reply) {
            (Some((v, _)), _) => v.session.clone(),
            (None, Some((_, _, session))) => Some(session.clone()),
            (None, None) => origin
                .as_ref()
                .and_then(|e| (e.read(cx).intelligence.as_ref()).map(|l| l.session.downgrade())),
        };
        let open_paths: Vec<_> = self
            .file_tabs
            .iter()
            .map(|tab| tab.editor.read(cx).absolute_path().to_owned())
            .collect();
        self.navigation.request += 1;
        let request = self.navigation.request;
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
                    // Installed dependencies are read-only too, like sources outside the workspace.
                    let relative = relative.filter(|p| {
                        !p.split('/')
                            .any(|c| c == "node_modules" || c == "site-packages")
                    });
                    let existing = open_paths
                        .into_iter()
                        .find(|p| p == &path || p.canonicalize().is_ok_and(|p| p == canonical));
                    Ok::<_, anyhow::Error>((canonical, path, text, relative, existing))
                })
                .await;
            let _ = this.update_in(cx, |s, window, cx| {
                if s.navigation.request != request {
                    return;
                }
                let steps = s.navigation.steps(request);
                s.navigation.pending = None;
                if s.active != workspace || s.editor != origin || s.tab != origin_tab {
                    return;
                }
                let (canonical, original, text, relative, existing_path) = match read {
                    Ok(read) => read,
                    Err(error) => {
                        // A deleted file leaves history: Back goes on to the entry before it.
                        let gone = (error.downcast_ref::<std::io::Error>())
                            .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound);
                        if let Some((_, forward)) = visit.as_ref().filter(|_| gone) {
                            let nav = &mut s.navigation;
                            let from = if *forward {
                                &mut nav.forward
                            } else {
                                &mut nav.back
                            };
                            if let Some(ix) = from.len().checked_sub(steps) {
                                from.remove(ix);
                                if ix > 0 {
                                    nav.pending = Some((*forward, steps - 1, request));
                                    return s.navigate_history(*forward, window, cx);
                                }
                            }
                        }
                        return notify_err(window, cx, error.to_string());
                    }
                };
                let existing =
                    s.find_tab(existing_path.as_deref(), &original, relative.as_deref(), cx);
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
                // An open file opened without a location keeps its cursor.
                let selected = match range {
                    Some((range, encoding)) => match (
                        document::offset(&source, range.start, encoding),
                        document::offset(&source, range.end, encoding),
                    ) {
                        (Ok(a), Ok(b)) if a <= b => Some(a..b),
                        _ => {
                            return notify_err(
                                window,
                                cx,
                                "The target location no longer matches its document",
                            );
                        }
                    },
                    None => visit.as_ref().map(|(v, _)| {
                        source.floor_char_boundary(v.selection.start.min(source.len()))
                            ..source.floor_char_boundary(v.selection.end.min(source.len()))
                    }),
                };
                let editor = match existing {
                    Some(ix) => s.file_tabs[ix].editor.clone(),
                    None => {
                        let external = relative.is_none();
                        let path = relative.unwrap_or_else(|| canonical.display().to_string());
                        let replace = (s.editor.as_ref())
                            .filter(|e| !new_tab && !e.read(cx).dirty && !e.read(cx).saving)
                            .and_then(|e| {
                                let ix = s.file_tabs.iter().position(|t| t.editor == *e)?;
                                let old = e.read(cx);
                                let slot = s.file_tabs[ix].slot;
                                Some((ix, TabId::File(old.path.clone()), old.external, slot))
                            });
                        let mut tab =
                            s.make_file_tab(path.clone(), original, Some(text), window, cx);
                        tab.editor.update(cx, |e, cx| {
                            e.set_external(external, cx);
                            if let Some(session) = session.and_then(|s| s.upgrade())
                                && external
                            {
                                e.attach_language(session, window, cx);
                            }
                        });
                        let editor = tab.editor.clone();
                        let new = TabId::File(path);
                        let ws = s.active_ws_mut().unwrap();
                        match replace {
                            Some((ix, old_tab, old_external, slot)) => {
                                // An external file sits after the tabs; it remembers the slot
                                // of the file it replaced, so going back restores it there.
                                match (old_external, external) {
                                    (false, false) => ws.replace_tab(&old_tab, new),
                                    (false, true) => {
                                        tab.slot = ws.tab_order.iter().position(|t| *t == old_tab);
                                        ws.close_tab(&old_tab);
                                    }
                                    (true, false) => match slot {
                                        Some(slot) => {
                                            ws.tab_order.retain(|t| *t != new);
                                            let slot = slot.min(ws.tab_order.len());
                                            ws.tab_order.insert(slot, new);
                                        }
                                        None => ws.open_tab(new),
                                    },
                                    (true, true) => tab.slot = slot,
                                }
                                s.file_tabs[ix] = tab;
                            }
                            None => {
                                if !external {
                                    ws.open_tab(new);
                                }
                                s.file_tabs.push(tab);
                            }
                        }
                        editor
                    }
                };
                if let Some((_, forward)) = &visit {
                    let (from, to) = if *forward {
                        (&mut s.navigation.forward, &mut s.navigation.back)
                    } else {
                        (&mut s.navigation.back, &mut s.navigation.forward)
                    };
                    // The target, then the entries quick repeated presses stepped over.
                    let passed = from.split_off(from.len().saturating_sub(steps));
                    if let Some(old) = old {
                        to.push(old);
                    }
                    to.extend(passed.into_iter().skip(1).rev());
                } else if let Some(old) = old {
                    s.navigation.back.push(old);
                    s.navigation.forward.clear();
                }
                let focus = reply.as_ref().is_none_or(|(_, focus, _)| *focus);
                if focus {
                    s.editor = Some(editor.clone());
                    s.show(Tab::File, window, cx);
                }
                editor.update(cx, |e, cx| {
                    e.input.update(cx, |i, cx| {
                        if let Some(selected) = selected {
                            i.set_selected_range(selected, cx);
                        }
                        if let Some((visit, _)) = visit {
                            i.set_scroll_offset(visit.scroll, cx);
                        }
                        if focus {
                            i.focus(window, cx);
                        }
                    })
                });
                if let Some((reply, ..)) = reply {
                    reply.respond(json!({"success":true}));
                }
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
        // Ropes clone cheaply; only the edit's own documents are compared off the UI thread.
        let open: Vec<_> = self
            .file_tabs
            .iter()
            .map(|tab| {
                let e = tab.editor.read(cx);
                (
                    e.absolute_path().to_owned(),
                    e.input.read(cx).text().clone(),
                    e.intelligence.as_ref().map(|l| l.snapshot.clone()),
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
                                Some((_, text, snapshot, blocked, fingerprint)) => {
                                    anyhow::ensure!(
                                        !blocked,
                                        "Resolve the save or disk conflict in {relative} first"
                                    );
                                    anyhow::ensure!(
                                        crate::files::fingerprint(disk.as_bytes()) == *fingerprint,
                                        "{relative} changed on disk; reload before applying edits"
                                    );
                                    let text = text.to_string();
                                    let version = snapshot.as_ref().and_then(|s| {
                                        let s = s.lock().unwrap();
                                        (s.text.as_ref() == text).then_some(s.version)
                                    });
                                    (text, version)
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
                            Ok(Staged {
                                path,
                                existing_path: existing.map(|(path, ..)| path.clone()),
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
                        let existing = s.find_tab(
                            edit.existing_path.as_deref(),
                            &edit.path,
                            Some(&edit.relative),
                            cx,
                        );
                        if let Some(ix) = existing {
                            let e = s.file_tabs[ix].editor.read(cx);
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
                                edit.existing_path.is_none() && edit.version.is_none(),
                                "A target editor closed; request the edit again"
                            );
                        }
                    }
                    for edit in staged {
                        if edit.before == edit.after {
                            continue;
                        }
                        let existing = s.find_tab(
                            edit.existing_path.as_deref(),
                            &edit.path,
                            Some(&edit.relative),
                            cx,
                        );
                        let editor = match existing {
                            Some(ix) => s.file_tabs[ix].editor.clone(),
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

/// FormattingOptions matching `text`'s own indentation: tabs when most indented lines start
/// with one, else its most common indent step (4 when nothing is indented). Servers such as
/// tsserver reindent the whole file to these options.
pub(super) fn formatting_options(text: &str) -> Value {
    let (mut tabs, mut spaces, mut previous) = (0, 0, 0);
    let mut steps = [0usize; 9];
    let mut in_comment = false;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        // Block-comment bodies (" * doc", " */") sit one space in; they are not a step.
        let trimmed = line.trim();
        if in_comment {
            in_comment = !trimmed.contains("*/");
            continue;
        }
        // Only a line-leading opener: `"/*"` in a string or `// /*` must not hide the rest.
        if let Some(body) = trimmed.strip_prefix("/*") {
            in_comment = !body.contains("*/");
        }
        if line.starts_with('\t') {
            tabs += 1;
            continue;
        }
        let indent = line.len() - line.trim_start_matches(' ').len();
        spaces += usize::from(indent > 0);
        if let Some(step) = indent.checked_sub(previous).filter(|s| (1..=8).contains(s)) {
            steps[step] += 1;
        }
        previous = indent;
    }
    let tab_size = (1..=8)
        .filter(|&s| steps[s] > 0)
        .max_by_key(|&s| (steps[s], std::cmp::Reverse(s)))
        .unwrap_or(4);
    json!({"tabSize": tab_size, "insertSpaces": tabs <= spaces})
}

pub(super) fn read_source(path: &Path) -> anyhow::Result<String> {
    use std::io::Read;
    // Before opening: opening a FIFO blocks until something writes to it.
    anyhow::ensure!(path.metadata()?.is_file(), "Not a regular source file");
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
