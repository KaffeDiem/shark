use super::*;
use crate::lsp::{
    Session, SessionEvent,
    client::Client,
    document::{self, Encoding, Snapshot},
    providers::{Provider, ProviderEvent},
};
use lsp_types::{LocationLink, PublishDiagnosticsParams, Uri, WorkspaceEdit};
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

pub enum LanguageEvent {
    /// Locations to open; true opens a new tab rather than replacing the current one.
    Navigate(Vec<LocationLink>, Encoding, bool),
    Apply {
        edit: WorkspaceEdit,
        snapshot: Snapshot,
        encoding: Encoding,
        guard: document::EditGuard,
    },
}

/// A document symbol, flattened in document order; `depth` nests it under the symbols before it.
/// Byte offsets into the text the server saw.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Symbol {
    pub name: String,
    pub depth: usize,
    pub range: std::ops::Range<usize>,
    pub target: usize,
}

/// Flattens a `textDocument/documentSymbol` result (nested `DocumentSymbol`s or flat
/// `SymbolInformation`s) and nests it by range, so both shapes give the same tree.
pub(crate) fn outline(value: &serde_json::Value, text: &str, encoding: Encoding) -> Vec<Symbol> {
    fn walk(value: &serde_json::Value, text: &str, encoding: Encoding, out: &mut Vec<Symbol>) {
        for symbol in value.as_array().into_iter().flatten() {
            let range = |v: &serde_json::Value| {
                let r: lsp_types::Range = serde_json::from_value(v.clone()).ok()?;
                let a = document::offset(text, r.start, encoding).ok()?;
                let b = document::offset(text, r.end, encoding).ok()?;
                (a <= b).then_some(a..b)
            };
            let full = range(&symbol["range"]).or_else(|| range(&symbol["location"]["range"]));
            if let Some(full) = full {
                let target = range(&symbol["selectionRange"]).map_or(full.start, |r| r.start);
                let name = symbol["name"].as_str().unwrap_or("Symbol").to_string();
                out.push(Symbol {
                    name,
                    depth: 0,
                    range: full,
                    target,
                });
            }
            walk(&symbol["children"], text, encoding, out);
        }
    }
    let mut out = vec![];
    walk(value, text, encoding, &mut out);
    out.sort_by_key(|s| (s.range.start, std::cmp::Reverse(s.range.end)));
    let mut open: Vec<usize> = vec![];
    for s in &mut out {
        while open.last().is_some_and(|&end| s.range.start >= end) {
            open.pop();
        }
        s.depth = open.len();
        open.push(s.range.end);
    }
    out
}

/// Indices of the symbols around `cursor`, outermost first.
pub(crate) fn symbol_path(outline: &[Symbol], cursor: usize) -> Vec<usize> {
    let mut path: Vec<usize> = vec![];
    for (ix, s) in outline.iter().enumerate() {
        if s.range.contains(&cursor) || (s.range.end == cursor && !s.range.is_empty()) {
            path.retain(|&p| outline[p].depth < s.depth);
            path.push(ix);
        }
    }
    path
}

pub(crate) struct Language {
    pub session: Entity<Session>,
    pub uri: Uri,
    pub snapshot: Arc<Mutex<Snapshot>>,
    pub client: Option<Client>,
    pub companion: Option<Client>,
    generation: u64,
    version: i32,
    alive: Arc<AtomicBool>,
    saved: Arc<Mutex<Option<String>>>,
    diagnostics: [Vec<gpui_kit::base::input::Diagnostic>; 2],
    sync: Task<()>,
    _events: Task<()>,
    _subs: Vec<Subscription>,
}

impl Drop for Language {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        let _guard = self.snapshot.lock().unwrap();
        if let Some(client) = &self.client {
            let _ = client.notify(
                "textDocument/didClose",
                json!({"textDocument":{"uri":self.uri}}),
            );
        }
        if let Some(client) = &self.companion {
            let _ = client.notify(
                "textDocument/didClose",
                json!({"textDocument":{"uri":self.uri}}),
            );
        }
    }
}

impl FileEditor {
    pub(crate) fn clear_language_diagnostics(&mut self) {
        if let Some(language) = &mut self.intelligence {
            language.diagnostics = Default::default();
        }
    }
    pub(crate) fn language_status(&self, cx: &App) -> Option<(String, Option<String>)> {
        if let Some(error) = &self.lsp_error {
            return Some(("Setup needed".into(), Some(error.clone())));
        }
        self.intelligence.as_ref().map(|l| {
            let s = l.session.read(cx);
            (s.status.clone(), s.detail.clone())
        })
    }
    /// Busy work of the language server, for the loading indicator.
    pub(crate) fn language_activity(&self, cx: &App) -> Option<crate::lsp::Work> {
        (self.lsp_error.is_none())
            .then(|| self.intelligence.as_ref()?.session.read(cx).activity())
            .flatten()
    }

    /// Recomputes the symbols around the cursor; notifies only when they change.
    pub(crate) fn sync_crumbs(&mut self, cx: &mut Context<Self>) {
        let crumbs = symbol_path(&self.outline, self.input.read(cx).cursor());
        if crumbs != self.crumbs {
            self.crumbs = crumbs;
            cx.notify();
        }
    }

    pub(crate) fn absolute_path(&self) -> &std::path::Path {
        &self.abs
    }
    pub(crate) fn disk_fingerprint(&self) -> u64 {
        self.disk
    }

    pub(crate) fn set_external(&mut self, external: bool, cx: &mut Context<Self>) {
        self.external = external;
        self.sync_readonly(cx);
    }

    pub(crate) fn attach_language(
        &mut self,
        session: Entity<Session>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(url) = url::Url::from_file_path(&self.abs) else {
            return;
        };
        let Ok(uri) = url.as_str().parse() else {
            return;
        };
        let subs = vec![
            cx.observe_in(&session, window, |e, session, w, cx| {
                if e.intelligence.as_ref().is_some_and(|l| {
                    l.generation != session.read(cx).generation || l.client.is_none()
                }) {
                    e.sync_language(w, cx);
                }
                cx.notify();
            }),
            cx.subscribe_in(
                &session,
                window,
                |e, _, event: &SessionEvent, window, cx| {
                    if let SessionEvent::Notification { method, .. } = event
                        && method == "workspace/diagnostic/refresh"
                    {
                        e.sync_language(window, cx);
                    }
                    if let SessionEvent::Notification {
                        method,
                        params,
                        companion,
                    } = event
                        && method == "textDocument/publishDiagnostics"
                        && let Ok(params) =
                            serde_json::from_value::<PublishDiagnosticsParams>(params.clone())
                    {
                        e.language_diagnostics(params, *companion, cx);
                    }
                },
            ),
        ];
        self.intelligence = Some(Language {
            session,
            uri,
            snapshot: Arc::new(Mutex::new(Snapshot {
                text: Arc::from(""),
                version: 0,
            })),
            client: None,
            companion: None,
            generation: 0,
            version: 0,
            sync: Task::ready(()),
            alive: Arc::new(AtomicBool::new(true)),
            saved: Arc::new(Mutex::new(None)),
            diagnostics: Default::default(),
            _events: Task::ready(()),
            _subs: subs,
        });
        self.lsp_error = None;
        self.sync_language(window, cx);
    }

    pub(crate) fn sync_language(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.loaded {
            return;
        }
        let Some(language) = &mut self.intelligence else {
            return;
        };
        let (generation, ready, companion, language_id) = {
            let session = language.session.read(cx);
            (
                session.generation,
                session.client.clone(),
                session.companion.clone(),
                crate::lsp::tools::language_id(&self.abs).unwrap_or(session.project.language_id),
            )
        };
        if language.generation != generation {
            self.lsp_error = None;
            language.alive.store(false, Ordering::Release);
            let _guard = language.snapshot.lock().unwrap();
            if let Some(old) = language.client.take() {
                let _ = old.notify(
                    "textDocument/didClose",
                    json!({"textDocument":{"uri":language.uri}}),
                );
            }
            if let Some(old) = language.companion.take() {
                let _ = old.notify(
                    "textDocument/didClose",
                    json!({"textDocument":{"uri":language.uri}}),
                );
            }
            language.sync = Task::ready(());
            language.generation = generation;
            drop(_guard);
            language.snapshot = Arc::new(Mutex::new(Snapshot {
                text: Arc::from(""),
                version: 0,
            }));
            language.alive = Arc::new(AtomicBool::new(true));
            language.diagnostics = Default::default();
            self.outline.clear();
            self.crumbs.clear();
            self.input.update(cx, |i, cx| {
                *i.lsp_mut() = Default::default();
                if let Some(diagnostics) = i.diagnostics_mut() {
                    diagnostics.clear();
                }
                cx.notify();
            });
        }
        let Some(client) = ready else { return };
        let opening = language.client.is_none();
        if opening {
            language.client = Some(client.clone());
            language.companion = companion.clone();
            let (tx, rx) = async_channel::unbounded();
            let provider = Rc::new(Provider::new(
                client.clone(),
                language.uri.clone(),
                language.snapshot.clone(),
                tx.clone(),
            ));
            let ruff = companion.as_ref().map(|c| {
                Rc::new(Provider::new(
                    c.clone(),
                    language.uri.clone(),
                    language.snapshot.clone(),
                    tx,
                ))
            });
            self.input.update(cx, |i, _| {
                let lsp = i.lsp_mut();
                lsp.completion_provider = Some(provider.clone());
                lsp.hover_provider = Some(provider.clone());
                lsp.definition_provider = Some(provider.clone());
                lsp.code_action_providers = vec![provider.clone()];
                if let Some(ruff) = ruff {
                    lsp.code_action_providers.push(ruff);
                }
                lsp.show_document = Some(provider.show_document_handler());
            });
            let encoding = Encoding::from(client.encoding());
            language._events = cx.spawn_in(window, async move |this, cx| {
                while let Ok(event) = rx.recv().await {
                    if this
                        .update(cx, |_, cx| {
                            cx.emit(EditorEvent::Language(match event {
                                ProviderEvent::Navigate(links, new_tab) => {
                                    LanguageEvent::Navigate(links, encoding, new_tab)
                                }
                                ProviderEvent::Apply {
                                    edit,
                                    snapshot,
                                    encoding,
                                    guard,
                                } => LanguageEvent::Apply {
                                    edit,
                                    snapshot,
                                    encoding,
                                    guard,
                                },
                            }));
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            });
        }
        language.version += 1;
        let version = language.version;
        let uri = language.uri.clone();
        let snapshot = language.snapshot.clone();
        let alive = language.alive.clone();
        let saved = language.saved.clone();
        let rope = self.input.read(cx).text().clone();
        language.sync = cx.spawn_in(window, async move |this, cx| {
            if !opening { cx.background_executor().timer(Duration::from_millis(35)).await; }
            let diagnostics = cx.background_executor().spawn(async move {
                let text: Arc<str> = Arc::from(rope.to_string());
                loop {
                let previous = snapshot.lock().unwrap().clone();
                if previous.version >= version { return Ok::<_, anyhow::Error>(None); }
                let encoding = Encoding::from(client.encoding());
                let ruff_change = companion.as_ref().map(|c| document::incremental_change(&previous.text, &text, c.encoding().into())).transpose()?.flatten();
                let change = if previous.version == 0 { None } else {
                    document::incremental_change(&previous.text, &text, encoding)?
                };
                let mut current = snapshot.lock().unwrap();
                if !alive.load(Ordering::Acquire) { return Ok(None); }
                if current.version != previous.version { continue; }
                if previous.version == 0 {
                    client.notify("textDocument/didOpen", json!({"textDocument":{"uri":uri,"languageId":language_id,"version":version,"text":text.as_ref()}}))?;
                    if let Some(ruff) = &companion { ruff.notify("textDocument/didOpen", json!({"textDocument":{"uri":uri,"languageId":language_id,"version":version,"text":text.as_ref()}}))?; }
                } else if previous.text != text {
                    let kind = client.capabilities().and_then(|c| c.text_document_sync).map(|s| match s {
                        lsp_types::TextDocumentSyncCapability::Kind(k) => k,
                        lsp_types::TextDocumentSyncCapability::Options(o) => o.change.unwrap_or(lsp_types::TextDocumentSyncKind::NONE),
                    }).unwrap_or(lsp_types::TextDocumentSyncKind::NONE);
                    let change = if kind == lsp_types::TextDocumentSyncKind::INCREMENTAL {
                        change
                    } else if kind == lsp_types::TextDocumentSyncKind::FULL {
                        Some(lsp_types::TextDocumentContentChangeEvent { range: None, range_length: None, text: text.to_string() })
                    } else { None };
                    if let Some(change) = change {
                        client.notify("textDocument/didChange", json!({"textDocument":{"uri":uri,"version":version},"contentChanges":[change]}))?;
                    }
                    if let (Some(ruff), Some(change)) = (&companion, ruff_change) { ruff.notify("textDocument/didChange", json!({"textDocument":{"uri":uri,"version":version},"contentChanges":[change]}))?; }
                }
                if previous.version == 0 || previous.text != text {
                    *current = Snapshot { text, version };
                }
                if let Some(saved) = saved.lock().unwrap().take() {
                    notify_save(&client, &uri, &saved)?;
                    if let Some(ruff) = &companion { notify_save(ruff, &uri, &saved)?; }
                }
                let version = current.version;
                let text = current.text.clone();
                let symbols = Some(client.clone()).filter(|c| c.capabilities().and_then(|c| c.document_symbol_provider).is_some_and(|p| !matches!(p, lsp_types::OneOf::Left(false))));
                let clients: Vec<_> = [(Some(client), false), (companion, true)].into_iter()
                    .filter_map(|(client, companion)| client.filter(|c| c.capabilities().is_some_and(|c| c.diagnostic_provider.is_some())).map(|c| (c, companion)))
                    .collect();
                return Ok(Some((clients, uri, version, symbols, text)));
                }
            }).await;
            match diagnostics {
                Ok(Some((clients, uri, version, symbols, text))) => {
                    for (client, companion) in clients {
                    if let Ok(report) = client.request("textDocument/diagnostic", json!({"textDocument":{"uri":uri}})).await
                        && report["kind"] == "full"
                        && let Ok(items) = serde_json::from_value(report["items"].clone()) {
                        let _ = this.update(cx, |e, cx| e.language_diagnostics(PublishDiagnosticsParams { uri: uri.clone(), diagnostics: items, version: Some(version) }, companion, cx));
                    }
                    }
                    // A newer edit replaces this task, so a stale outline never lands.
                    if let Some(client) = symbols
                        && let Ok(result) = client.request("textDocument/documentSymbol", json!({"textDocument":{"uri":uri}})).await {
                        let encoding = Encoding::from(client.encoding());
                        let symbols = cx.background_executor().spawn(async move { outline(&result, &text, encoding) }).await;
                        let _ = this.update(cx, |e, cx| { e.outline = symbols; e.sync_crumbs(cx); cx.notify(); });
                    }
                }
                Ok(None) => {}
                Err(error) => { let _ = this.update(cx, |e, cx| { e.lsp_error = Some(error.to_string()); cx.notify(); }); }
            }
        });
    }

    fn language_diagnostics(
        &mut self,
        mut params: PublishDiagnosticsParams,
        companion: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(language) = &mut self.intelligence else {
            return;
        };
        if params.uri != language.uri {
            return;
        }
        let Some(client) = (if companion {
            &language.companion
        } else {
            &language.client
        }) else {
            return;
        };
        let snapshot = language.snapshot.lock().unwrap().clone();
        if params.version.is_some_and(|v| v != snapshot.version)
            || self.input.read(cx).value().as_ref() != snapshot.text.as_ref()
        {
            return;
        }
        let encoding = Encoding::from(client.encoding());
        params.diagnostics.retain_mut(|d| {
            match document::convert_range(&snapshot.text, d.range, encoding, Encoding::Utf32) {
                Ok(range) => {
                    d.range = range;
                    true
                }
                Err(_) => false,
            }
        });
        language.diagnostics[usize::from(companion)] =
            params.diagnostics.into_iter().map(Into::into).collect();
        let mut combined: Vec<_> = language.diagnostics.iter().flatten().cloned().collect();
        // GPUI's diagnostic tree requires document order, including across servers.
        combined.sort_by_key(|d| (d.range.start, d.range.end));
        self.input.update(cx, |i, cx| {
            if let Some(diagnostics) = i.diagnostics_mut() {
                diagnostics.clear();
                diagnostics.extend(combined);
            }
            cx.notify();
        });
    }

    pub(crate) fn language_saved(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(language) = &self.intelligence else {
            return;
        };
        *language.saved.lock().unwrap() = Some(text.into());
        self.sync_language(window, cx);
    }

    pub(crate) fn replace_language_text(
        &mut self,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        document::document_changed();
        self.input.update(cx, |i, cx| {
            let selection = i.selected_range();
            let scroll = i.scroll_offset();
            let start = text.floor_char_boundary(selection.start.min(text.len()));
            let end = text.floor_char_boundary(selection.end.min(text.len()));
            i.replace_all(text, window, cx);
            i.set_selected_range(start..end, cx);
            i.set_scroll_offset(scroll, cx);
        });
        self.dirty = true;
        self.sync_language(window, cx);
        cx.notify();
    }
}

fn notify_save(client: &Client, uri: &Uri, text: &str) -> anyhow::Result<()> {
    let save = client
        .capabilities()
        .and_then(|c| c.text_document_sync)
        .and_then(|sync| match sync {
            lsp_types::TextDocumentSyncCapability::Options(options) => options.save,
            _ => None,
        });
    let include_text = match save {
        Some(lsp_types::TextDocumentSyncSaveOptions::Supported(true)) => false,
        Some(lsp_types::TextDocumentSyncSaveOptions::SaveOptions(options)) => {
            options.include_text.unwrap_or(false)
        }
        _ => return Ok(()),
    };
    let mut params = json!({"textDocument":{"uri":uri}});
    if include_text {
        params["text"] = json!(text);
    }
    client.notify("textDocument/didSave", params)
}
