//! Adapters for GPUI's existing completion, hover, definition and quick-fix overlays.
use super::{
    client::Client,
    document::{self, EditGuard, Encoding, Snapshot},
};
use anyhow::{Result, bail, ensure};
use gpui_kit::base::input::{
    CodeActionProvider, CompletionProvider, DefinitionProvider, EditorState, HoverProvider, Rope,
    ShowDocumentHandler,
};
use gpui_kit::component::WindowExt as _;
use gpui_kit::{App, BackgroundExecutor, Entity, Task, Window};
use lsp_types::*;
use serde_json::json;
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, Mutex},
    time::Duration,
};

pub enum ProviderEvent {
    Navigate(Vec<LocationLink>),
    Apply {
        edit: WorkspaceEdit,
        snapshot: Snapshot,
        encoding: Encoding,
        guard: EditGuard,
    },
}

#[derive(Clone)]
pub struct Provider {
    id: gpui_kit::SharedString,
    client: Client,
    uri: Uri,
    snapshot: Arc<Mutex<Snapshot>>,
    events: async_channel::Sender<ProviderEvent>,
    definitions: Rc<RefCell<Vec<LocationLink>>>,
}

impl Provider {
    pub fn new(
        client: Client,
        uri: Uri,
        snapshot: Arc<Mutex<Snapshot>>,
        events: async_channel::Sender<ProviderEvent>,
    ) -> Self {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            id: format!(
                "shark-lsp-{}",
                NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            )
            .into(),
            client,
            uri,
            snapshot,
            events,
            definitions: Rc::default(),
        }
    }

    pub fn encoding(&self) -> Encoding {
        self.client.encoding().into()
    }

    async fn synchronized(&self, text: &str, executor: &BackgroundExecutor) -> Result<Snapshot> {
        for _ in 0..100 {
            let snapshot = self.snapshot.lock().unwrap().clone();
            if snapshot.text.as_ref() == text {
                return Ok(snapshot);
            }
            // A previous flush can land first; keep waiting for this request's exact text.
            executor.timer(Duration::from_millis(10)).await;
        }
        bail!("document synchronization timed out")
    }

    fn current(&self, snapshot: &Snapshot) -> Result<()> {
        ensure!(
            *self.snapshot.lock().unwrap() == *snapshot,
            "language response is stale"
        );
        Ok(())
    }

    fn params(&self, text: &str, offset: usize) -> Result<serde_json::Value> {
        Ok(
            json!({"textDocument":{"uri":self.uri}, "position":document::position(text,offset,self.encoding())?}),
        )
    }

    /// The hook receives original server coordinates; the location-aware opener converts them.
    pub fn show_document_handler(&self) -> ShowDocumentHandler {
        let definitions = self.definitions.clone();
        let events = self.events.clone();
        Rc::new(move |params, _, _| {
            let cached = definitions.borrow();
            let links = if cached.iter().any(|l| {
                l.target_uri == params.uri && Some(l.target_selection_range) == params.selection
            }) {
                cached.clone()
            } else {
                let range = params.selection.unwrap_or_default();
                vec![LocationLink {
                    origin_selection_range: None,
                    target_uri: params.uri.clone(),
                    target_range: range,
                    target_selection_range: range,
                }]
            };
            let _ = events.try_send(ProviderEvent::Navigate(links));
            true
        })
    }
}

impl CompletionProvider for Provider {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        mut trigger: CompletionContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let this = self.clone();
        let text = text.clone();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |_| {
            let text = executor.spawn(async move { text.to_string() }).await;
            let snapshot = this.synchronized(&text, &executor).await?;
            let guard = EditGuard::capture();
            if trigger.trigger_kind == CompletionTriggerKind::TRIGGER_CHARACTER {
                let configured = this
                    .client
                    .capabilities()
                    .and_then(|c| c.completion_provider)
                    .and_then(|p| p.trigger_characters)
                    .unwrap_or_default();
                if !trigger
                    .trigger_character
                    .as_ref()
                    .is_some_and(|c| configured.contains(c))
                {
                    trigger.trigger_kind = CompletionTriggerKind::INVOKED;
                    trigger.trigger_character = None;
                }
            }
            let mut params = this.params(&text, offset)?;
            params["context"] = serde_json::to_value(trigger)?;
            let response = this
                .client
                .request("textDocument/completion", params)
                .await?;
            this.current(&snapshot)?;
            ensure!(guard.is_current(), "completion response is stale");
            if response.is_null() {
                return Ok(CompletionResponse::Array(vec![]));
            }
            let response: CompletionResponse = serde_json::from_value(response)?;
            let mut items = match response {
                CompletionResponse::Array(items) => items,
                CompletionResponse::List(list) => list.items,
            };
            if this
                .client
                .capabilities()
                .and_then(|c| c.completion_provider)
                .is_some_and(|p| p.resolve_provider == Some(true))
            {
                // shortcut: resolve before showing the menu; switch to lazy resolution when GPUI exposes an accepted-item hook.
                for chunk in items.chunks_mut(8) {
                    this.current(&snapshot)?;
                    ensure!(guard.is_current(), "completion response is stale");
                    let tasks: Vec<_> = chunk
                        .iter()
                        .enumerate()
                        .filter(|(_, item)| {
                            item.data.is_some() && item.additional_text_edits.is_none()
                        })
                        .map(|(index, item)| {
                            let client = this.client.clone();
                            let item = item.clone();
                            (
                                index,
                                executor.spawn(async move {
                                    let response = client
                                        .request(
                                            "completionItem/resolve",
                                            serde_json::to_value(item)?,
                                        )
                                        .await?;
                                    serde_json::from_value::<CompletionItem>(response)
                                        .map_err(anyhow::Error::from)
                                }),
                            )
                        })
                        .collect();
                    for (index, task) in tasks {
                        chunk[index] = task.await?;
                    }
                }
                this.current(&snapshot)?;
            }
            let encoding = this.encoding();
            let items = executor
                .spawn(async move {
                    items
                        .into_iter()
                        .filter_map(|item| {
                            prepare_completion(&snapshot, offset, item, encoding).ok()
                        })
                        .collect()
                })
                .await;
            ensure!(guard.is_current(), "completion response is stale");
            Ok(CompletionResponse::Array(items))
        })
    }

    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        let configured = self
            .client
            .capabilities()
            .and_then(|c| c.completion_provider)
            .and_then(|p| p.trigger_characters)
            .unwrap_or_default();
        !new_text.is_empty()
            && (new_text.chars().all(|c| c.is_alphanumeric() || c == '_')
                || configured.iter().any(|c| c == new_text))
    }
}

impl HoverProvider for Provider {
    fn hover(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Option<Hover>>> {
        let this = self.clone();
        let text = text.clone();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |_| {
            let text = executor.spawn(async move { text.to_string() }).await;
            let snapshot = this.synchronized(&text, &executor).await?;
            let guard = EditGuard::capture();
            let response = this
                .client
                .request("textDocument/hover", this.params(&text, offset)?)
                .await?;
            this.current(&snapshot)?;
            ensure!(guard.is_current(), "hover response is stale");
            let mut hover: Option<Hover> = serde_json::from_value(response)?;
            if let Some(hover) = hover.as_mut() {
                hover.range = hover
                    .range
                    .map(|range| {
                        document::convert_range(&text, range, this.encoding(), Encoding::Utf32)
                    })
                    .transpose()?;
            }
            Ok(hover)
        })
    }
}

impl DefinitionProvider for Provider {
    fn definitions(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Vec<LocationLink>>> {
        let this = self.clone();
        let text = text.clone();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |_| {
            let text = executor.spawn(async move { text.to_string() }).await;
            let snapshot = this.synchronized(&text, &executor).await?;
            let guard = EditGuard::capture();
            let response = this
                .client
                .request("textDocument/definition", this.params(&text, offset)?)
                .await?;
            this.current(&snapshot)?;
            ensure!(guard.is_current(), "definition response is stale");
            let response: Option<GotoDefinitionResponse> = serde_json::from_value(response)?;
            let links = match response {
                None => vec![],
                Some(GotoDefinitionResponse::Link(links)) => links,
                Some(GotoDefinitionResponse::Scalar(location)) => vec![location_link(location)],
                Some(GotoDefinitionResponse::Array(locations)) => {
                    locations.into_iter().map(location_link).collect()
                }
            };
            *this.definitions.borrow_mut() = links.clone();
            links
                .into_iter()
                .map(|mut link| {
                    link.origin_selection_range = link
                        .origin_selection_range
                        .map(|range| {
                            document::convert_range(&text, range, this.encoding(), Encoding::Utf32)
                        })
                        .transpose()?;
                    Ok(link)
                })
                .collect()
        })
    }
}

pub fn location_link(location: Location) -> LocationLink {
    LocationLink {
        origin_selection_range: None,
        target_uri: location.uri,
        target_range: location.range,
        target_selection_range: location.range,
    }
}

impl CodeActionProvider for Provider {
    fn id(&self) -> gpui_kit::SharedString {
        self.id.clone()
    }

    fn code_actions(
        &self,
        state: Entity<EditorState>,
        range: std::ops::Range<usize>,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Vec<CodeAction>>> {
        let this = self.clone();
        let text = state.read(cx).value().to_string();
        let guard = EditGuard::capture();
        let diagnostics: Vec<Diagnostic> = state
            .read(cx)
            .diagnostics()
            .map(|set| {
                set.range(range.clone())
                    .filter_map(|entry| {
                        let d = &entry.diagnostic;
                        let range = document::convert_range(
                            &text,
                            Range::new(d.range.start, d.range.end),
                            Encoding::Utf32,
                            this.encoding(),
                        )
                        .ok()?;
                        Some(Diagnostic {
                            range,
                            severity: Some(match d.severity {
                                gpui_kit::base::input::DiagnosticSeverity::Error => {
                                    DiagnosticSeverity::ERROR
                                }
                                gpui_kit::base::input::DiagnosticSeverity::Warning => {
                                    DiagnosticSeverity::WARNING
                                }
                                gpui_kit::base::input::DiagnosticSeverity::Info => {
                                    DiagnosticSeverity::INFORMATION
                                }
                                gpui_kit::base::input::DiagnosticSeverity::Hint => {
                                    DiagnosticSeverity::HINT
                                }
                            }),
                            code: d
                                .code
                                .as_ref()
                                .map(|c| NumberOrString::String(c.to_string())),
                            code_description: d.code_description.clone(),
                            source: d.source.as_ref().map(ToString::to_string),
                            message: d.message.to_string(),
                            related_information: d.related_information.clone(),
                            tags: d.tags.clone(),
                            data: d.data.clone(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |_| {
            let snapshot = this.synchronized(&text,&executor).await?;
            let range = Range::new(document::position(&text,range.start,this.encoding())?,document::position(&text,range.end,this.encoding())?);
            let response = this.client.request("textDocument/codeAction",json!({"textDocument":{"uri":this.uri},"range":range,"context":{"diagnostics":diagnostics}})).await?;
            this.current(&snapshot)?;
            let actions: Option<CodeActionResponse> = serde_json::from_value(response)?;
            Ok(actions.unwrap_or_default().into_iter().filter_map(|a| match a {
                CodeActionOrCommand::CodeAction(mut a) if a.disabled.is_none() => {
                    a.data = Some(json!({"sharkVersion":snapshot.version,"sharkGuard":guard,"serverData":a.data}));
                    Some(a)
                },
                _ => None,
            }).collect())
        })
    }

    fn perform_code_action(
        &self,
        _: Entity<EditorState>,
        mut action: CodeAction,
        _: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<()>> {
        let this = self.clone();
        let snapshot = self.snapshot.lock().unwrap().clone();
        window.spawn(cx, async move |cx| {
            let result: Result<()> = async {
            let data = action.data.take().unwrap_or_default();
            let guard: EditGuard = serde_json::from_value(data["sharkGuard"].clone())?;
            ensure!(guard.is_current(), "Workspace changed; request the quick fix again");
            ensure!(data["sharkVersion"].as_i64() == Some(snapshot.version as i64), "quick fix is stale");
            action.data = data.get("serverData").cloned().filter(|v| !v.is_null());
            if action.edit.is_none() && this.client.capabilities().and_then(|c| c.code_action_provider).is_some_and(|p| matches!(p, CodeActionProviderCapability::Options(o) if o.resolve_provider == Some(true))) {
                action = serde_json::from_value(this.client.request("codeAction/resolve",serde_json::to_value(action)?).await?)?;
            }
            this.current(&snapshot)?;
            ensure!(action.command.is_none(), "command-based quick fixes are not supported");
            let edit = action.edit.ok_or_else(|| anyhow::anyhow!("quick fix contains no supported edits"))?;
            document::workspace_edits(edit.clone())?;
            this.events.send(ProviderEvent::Apply { edit, snapshot, encoding: this.encoding(), guard }).await.map_err(|_| anyhow::anyhow!("editor closed"))?;
            Ok(())
            }.await;
            if let Err(error) = &result {
                let message = format!("Could not apply quick fix: {error}");
                let _ = cx.update(|window, cx| window.push_notification(message, cx));
            }
            result
        })
    }
}

/// Fold auto-imports and snippet defaults into GPUI's single undoable completion edit.
pub fn prepare_completion(
    snapshot: &Snapshot,
    cursor: usize,
    mut item: CompletionItem,
    encoding: Encoding,
) -> Result<CompletionItem> {
    ensure!(
        item.command.is_none(),
        "command-based completions are unsupported"
    );
    let (range, mut text) = match item.text_edit.take() {
        Some(CompletionTextEdit::Edit(edit)) => (edit.range, edit.new_text),
        Some(CompletionTextEdit::InsertAndReplace(edit)) => (edit.replace, edit.new_text),
        None => {
            ensure!(
                snapshot.text.is_char_boundary(cursor),
                "invalid completion cursor"
            );
            let start = snapshot.text[..cursor]
                .char_indices()
                .rev()
                .take_while(|(_, c)| c.is_alphanumeric() || *c == '_')
                .last()
                .map_or(cursor, |(i, _)| i);
            (
                Range::new(
                    document::position(&snapshot.text, start, encoding)?,
                    document::position(&snapshot.text, cursor, encoding)?,
                ),
                item.insert_text
                    .take()
                    .unwrap_or_else(|| item.label.clone()),
            )
        }
    };
    let primary_start = document::offset(&snapshot.text, range.start, encoding)?;
    let primary_end = document::offset(&snapshot.text, range.end, encoding)?;
    let mut tabstops = vec![];
    if item.insert_text_format == Some(InsertTextFormat::SNIPPET) {
        let snippet = document::snippet(&text)?;
        text = snippet.text;
        tabstops = snippet.tabstops;
    }
    let primary_len = text.len();
    let mut edits = item.additional_text_edits.take().unwrap_or_default();
    let mut first = primary_start;
    let mut last = primary_end;
    let mut shift: isize = 0;
    for edit in &edits {
        let start = document::offset(&snapshot.text, edit.range.start, encoding)?;
        let end = document::offset(&snapshot.text, edit.range.end, encoding)?;
        first = first.min(start);
        last = last.max(end);
        if end <= primary_start {
            shift += edit.new_text.len() as isize - (end - start) as isize;
        }
    }
    edits.push(TextEdit::new(range, text));
    // shortcut: each candidate copies the span between imports and insertion; stage only accepted edits once GPUI exposes an acceptance hook.
    let edited = document::apply_edits(snapshot, Some(snapshot.version), &edits, encoding)?;
    let new_end = edited.len() - (snapshot.text.len() - last);
    let primary_new_start = primary_start
        .checked_add_signed(shift)
        .ok_or_else(|| anyhow::anyhow!("invalid completion edits"))?;
    let stops: Vec<_> = tabstops.into_iter().map(|(index,range)| json!({"index":index,"start":primary_new_start+range.start,"end":primary_new_start+range.end})).collect();
    // The host can restore the primary caret and start tabstop navigation after acceptance.
    item.data = Some(
        json!({"sharkCompletion":{"version":snapshot.version,"fingerprint":crate::files::fingerprint(edited.as_bytes()),"cursor":primary_new_start+primary_len,"tabstops":stops},"serverData":item.data}),
    );
    item.text_edit = Some(CompletionTextEdit::Edit(TextEdit::new(
        Range::new(
            document::position(&snapshot.text, first, Encoding::Utf32)?,
            document::position(&snapshot.text, last, Encoding::Utf32)?,
        ),
        edited[first..new_end].to_owned(),
    )));
    item.insert_text_format = Some(InsertTextFormat::PLAIN_TEXT);
    Ok(item)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completion_combines_autoimport_and_unicode_snippet_as_one_edit() {
        let snapshot = Snapshot {
            text: Arc::from("😀\nfoo"),
            version: 7,
        };
        let item = CompletionItem {
            label: "foobar".into(),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            text_edit: Some(CompletionTextEdit::Edit(TextEdit::new(
                Range::new(Position::new(1, 0), Position::new(1, 3)),
                "foobar(${1:arg})$0".into(),
            ))),
            additional_text_edits: Some(vec![TextEdit::new(
                Range::default(),
                "import foobar\n".into(),
            )]),
            ..Default::default()
        };
        let item =
            prepare_completion(&snapshot, snapshot.text.len(), item, Encoding::Utf16).unwrap();
        let CompletionTextEdit::Edit(edit) = item.text_edit.unwrap() else {
            panic!()
        };
        let result = document::apply_edits(&snapshot, None, &[edit], Encoding::Utf32).unwrap();
        assert_eq!(result, "import foobar\n😀\nfoobar(arg)");
        assert_eq!(
            item.data.as_ref().unwrap()["sharkCompletion"]["fingerprint"].as_u64(),
            Some(crate::files::fingerprint(result.as_bytes()))
        );
        assert!(item.additional_text_edits.is_none());
    }
}
