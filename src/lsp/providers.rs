//! Adapters for GPUI's existing completion, hover, definition and quick-fix overlays.
use super::{
    client::Client,
    document::{self, CompletionMeta, EditGuard, Encoding, Lines, Snapshot, Stop},
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
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

/// Data-carrying items resolved before the menu shows; the rest are presented unresolved.
const EAGER_RESOLVE: usize = 16;

pub enum ProviderEvent {
    /// Definitions to open; true (shift held) opens a new tab.
    Navigate(Vec<LocationLink>, bool),
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
    /// Bumped by every change to this document, synchronized or not.
    edits: Arc<AtomicU64>,
    events: async_channel::Sender<ProviderEvent>,
    definitions: Rc<RefCell<Vec<LocationLink>>>,
}

impl Provider {
    pub fn new(
        client: Client,
        uri: Uri,
        snapshot: Arc<Mutex<Snapshot>>,
        edits: Arc<AtomicU64>,
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
            edits,
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

    fn edits(&self) -> u64 {
        self.edits.load(Ordering::Acquire)
    }

    /// Read-only responses go stale only when this document changes, including unsynced typing.
    fn unchanged(&self, snapshot: &Snapshot, edits: u64) -> Result<()> {
        self.current(snapshot)?;
        ensure!(self.edits() == edits, "language response is stale");
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
        Rc::new(move |params, window, _| {
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
            // cmd-click follows in the same tab; cmd-shift-click opens a new one.
            let new_tab = window.modifiers().shift;
            let _ = events.try_send(ProviderEvent::Navigate(links, new_tab));
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
            let edits = this.edits();
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
            this.unchanged(&snapshot, edits)?;
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
                // shortcut: resolve the first items before showing the menu; resolve only the accepted item once GPUI exposes an accepted-item hook.
                let resolved = Arc::new(Mutex::new(vec![]));
                let tasks: Vec<_> = eager_resolve(&items)
                    .into_iter()
                    .map(|index| {
                        let client = this.client.clone();
                        let item = items[index].clone();
                        let resolved = resolved.clone();
                        executor.spawn(async move {
                            let item = async {
                                let response = client
                                    .request("completionItem/resolve", serde_json::to_value(item)?)
                                    .await?;
                                anyhow::Ok(serde_json::from_value::<CompletionItem>(response)?)
                            }
                            .await;
                            resolved.lock().unwrap().push((index, item));
                        })
                    })
                    .collect();
                // Present what resolved within about 50 ms; dropping the rest cancels it.
                for _ in 0..10 {
                    if resolved.lock().unwrap().len() == tasks.len() {
                        break;
                    }
                    executor.timer(Duration::from_millis(5)).await;
                }
                drop(tasks);
                for (index, item) in resolved.lock().unwrap().drain(..) {
                    if let Ok(item) = item {
                        items[index] = item;
                    }
                }
                this.unchanged(&snapshot, edits)?;
            }
            let encoding = this.encoding();
            let items = executor
                .spawn(async move {
                    let lines = Lines::new(&snapshot.text);
                    items
                        .into_iter()
                        .filter_map(|item| {
                            prepare_completion(&snapshot, &lines, offset, item, encoding).ok()
                        })
                        .collect()
                })
                .await;
            ensure!(this.edits() == edits, "completion response is stale");
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
            let edits = this.edits();
            let response = this
                .client
                .request("textDocument/hover", this.params(&text, offset)?)
                .await?;
            this.unchanged(&snapshot, edits)?;
            let mut hover: Option<Hover> = serde_json::from_value(response)?;
            if let Some(hover) = hover.as_mut() {
                hover.range = hover
                    .range
                    .map(|range| {
                        document::convert_range(&text, range, this.encoding(), Encoding::Editor)
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
            let edits = this.edits();
            let response = this
                .client
                .request("textDocument/definition", this.params(&text, offset)?)
                .await?;
            this.unchanged(&snapshot, edits)?;
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
                            document::convert_range(&text, range, this.encoding(), Encoding::Editor)
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

// Caret lookups include diagnostics starting at the caret; the LSP request stays empty-range.
fn diagnostic_context(
    text: &str,
    set: &gpui_kit::base::input::DiagnosticSet,
    range: std::ops::Range<usize>,
    encoding: Encoding,
) -> Vec<Diagnostic> {
    set.range(range.start..range.end.max(range.start.saturating_add(1)))
        .filter_map(|entry| {
            let d = &entry.diagnostic;
            let range = document::convert_range(
                text,
                Range::new(d.range.start, d.range.end),
                Encoding::Editor,
                encoding,
            )
            .ok()?;
            Some(Diagnostic {
                range,
                severity: Some(match d.severity {
                    gpui_kit::base::input::DiagnosticSeverity::Error => DiagnosticSeverity::ERROR,
                    gpui_kit::base::input::DiagnosticSeverity::Warning => {
                        DiagnosticSeverity::WARNING
                    }
                    gpui_kit::base::input::DiagnosticSeverity::Info => {
                        DiagnosticSeverity::INFORMATION
                    }
                    gpui_kit::base::input::DiagnosticSeverity::Hint => DiagnosticSeverity::HINT,
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
        let diagnostics = state
            .read(cx)
            .diagnostics()
            .map(|set| diagnostic_context(&text, set, range.clone(), this.encoding()))
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

/// The first data-carrying items in menu order, which resolution may give auto-import edits.
fn eager_resolve(items: &[CompletionItem]) -> Vec<usize> {
    (0..items.len())
        .filter(|&i| items[i].data.is_some() && items[i].additional_text_edits.is_none())
        .take(EAGER_RESOLVE)
        .collect()
}

/// Fold auto-imports and snippet defaults into GPUI's single undoable completion edit.
pub fn prepare_completion(
    snapshot: &Snapshot,
    lines: &Lines,
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
                    lines.position(start, encoding)?,
                    lines.position(cursor, encoding)?,
                ),
                item.insert_text
                    .take()
                    .unwrap_or_else(|| item.label.clone()),
            )
        }
    };
    let offsets = |range: Range| -> Result<(usize, usize)> {
        let start = lines.offset(range.start, encoding)?;
        let end = lines.offset(range.end, encoding)?;
        ensure!(start <= end, "reversed edit range");
        Ok((start, end))
    };
    let (primary_start, primary_end) = offsets(range)?;
    let mut tabstops = vec![];
    if item.insert_text_format == Some(InsertTextFormat::SNIPPET) {
        let snippet = document::snippet(&text)?;
        text = snippet.text;
        tabstops = snippet.tabstops;
    }
    let primary_len = text.len();
    let mut edits = item
        .additional_text_edits
        .take()
        .unwrap_or_default()
        .into_iter()
        .map(|e| Ok((offsets(e.range)?, e.new_text)))
        .collect::<Result<Vec<_>>>()?;
    let shift: isize = edits
        .iter()
        .filter(|((_, end), _)| *end <= primary_start)
        .map(|((start, end), text)| text.len() as isize - (end - start) as isize)
        .sum();
    edits.push(((primary_start, primary_end), text));
    // Stable: same-position insertions keep array order, as in apply_edits.
    edits.sort_by_key(|(range, _)| *range);
    ensure!(
        edits.windows(2).all(|pair| pair[0].0.1 <= pair[1].0.0),
        "overlapping edits"
    );
    let first = edits[0].0.0;
    // shortcut: each candidate copies the span between imports and insertion; stage only accepted edits once GPUI exposes an acceptance hook.
    let mut folded = String::new();
    let mut at = first;
    for ((start, end), text) in &edits {
        folded.push_str(&snapshot.text[at..*start]);
        folded.push_str(text);
        at = *end;
    }
    let len = snapshot.text.len() - (at - first) + folded.len();
    ensure!(
        len as u64 <= crate::editor::MAX_BYTES,
        "edited file exceeds the 4 MiB editor limit"
    );
    let primary_new_start = primary_start
        .checked_add_signed(shift)
        .ok_or_else(|| anyhow::anyhow!("invalid completion edits"))?;
    // The host recognizes acceptance from the span alone, then restores the caret and tabstops.
    let meta = CompletionMeta {
        version: snapshot.version,
        start: first,
        end: first + folded.len(),
        fingerprint: crate::files::fingerprint(folded.as_bytes()),
        len,
        cursor: primary_new_start + primary_len,
        tabstops: tabstops
            .into_iter()
            .map(|(index, range)| Stop {
                index,
                start: primary_new_start + range.start,
                end: primary_new_start + range.end,
            })
            .collect(),
    };
    item.data = Some(json!({"sharkCompletion":meta,"serverData":item.data}));
    item.text_edit = Some(CompletionTextEdit::Edit(TextEdit::new(
        Range::new(
            lines.position(first, Encoding::Editor)?,
            lines.position(at, Encoding::Editor)?,
        ),
        folded,
    )));
    item.insert_text_format = Some(InsertTextFormat::PLAIN_TEXT);
    Ok(item)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_fix_context_includes_the_diagnostic_at_its_first_character() {
        use gpui_kit::base::input::{Diagnostic as EditorDiagnostic, DiagnosticSet};
        let text = "😀 unused\nnext";
        let mut set = DiagnosticSet::new(&Rope::from(text));
        set.push(EditorDiagnostic::new(
            Position::new(0, 2)..Position::new(0, 8),
            "unused name",
        ));
        for selection in [5..5, 6..6, 5..11] {
            let diagnostics = diagnostic_context(text, &set, selection, Encoding::Utf16);
            assert_eq!(diagnostics.len(), 1);
            assert_eq!(diagnostics[0].range.start, Position::new(0, 3));
            assert_eq!(diagnostics[0].range.end, Position::new(0, 9));
        }
        assert!(diagnostic_context(text, &set, 0..0, Encoding::Utf16).is_empty());
        assert!(diagnostic_context(text, &set, 13..13, Encoding::Utf16).is_empty());
    }

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
        let lines = Lines::new(&snapshot.text);
        let item = prepare_completion(
            &snapshot,
            &lines,
            snapshot.text.len(),
            item,
            Encoding::Utf16,
        )
        .unwrap();
        let CompletionTextEdit::Edit(edit) = item.text_edit.unwrap() else {
            panic!()
        };
        let result = document::apply_edits(&snapshot, None, &[edit], Encoding::Editor).unwrap();
        assert_eq!(result, "import foobar\n😀\nfoobar(arg)");
        let meta: CompletionMeta =
            serde_json::from_value(item.data.as_ref().unwrap()["sharkCompletion"].clone()).unwrap();
        assert_eq!(
            (meta.start, meta.end, meta.len),
            (0, result.len(), result.len())
        );
        assert_eq!(
            meta.fingerprint,
            crate::files::fingerprint(&result.as_bytes()[meta.start..meta.end])
        );
        assert_eq!(
            meta.tabstops[0],
            Stop {
                index: 1,
                start: 26,
                end: 29
            }
        );
        assert!(item.additional_text_edits.is_none());
    }

    #[test]
    fn completion_resolves_only_the_first_data_items_before_the_menu() {
        let item = |data: bool| CompletionItem {
            data: data.then(|| json!({})),
            ..Default::default()
        };
        let mut items: Vec<_> = (0..1000).map(|i| item(i != 1)).collect();
        items[2].additional_text_edits = Some(vec![]);
        let eager = eager_resolve(&items);
        assert_eq!(eager.len(), EAGER_RESOLVE);
        assert_eq!(&eager[..3], &[0, 3, 4]);
    }
}
