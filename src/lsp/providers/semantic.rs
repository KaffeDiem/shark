use super::*;
use gpui_kit::base::input::DocumentRangeSemanticTokensProvider;

impl Provider {
    pub fn semantic_options(&self) -> Option<SemanticTokensOptions> {
        let options = match self.client.capabilities()?.semantic_tokens_provider? {
            SemanticTokensServerCapabilities::SemanticTokensOptions(options) => options,
            SemanticTokensServerCapabilities::SemanticTokensRegistrationOptions(options) => {
                options.semantic_tokens_options
            }
        };
        (full_tokens(&options) || options.range == Some(true)).then_some(options)
    }
}

fn full_tokens(options: &SemanticTokensOptions) -> bool {
    matches!(
        options.full,
        Some(SemanticTokensFullOptions::Bool(true) | SemanticTokensFullOptions::Delta { .. })
    )
}

/// LSP names differ from the editor palette; preserve legend indices while translating names.
fn highlight_name(name: &str) -> &str {
    match name {
        "namespace" | "class" | "enum" | "interface" | "struct" | "typeParameter" => "type",
        "method" | "macro" => "function",
        "enumMember" => "constant",
        "parameter" => "variable.special",
        "event" => "property",
        "modifier" => "keyword",
        "regexp" => "string.regex",
        "decorator" => "attribute",
        name => name,
    }
}

impl DocumentRangeSemanticTokensProvider for Provider {
    fn legend(&self) -> SemanticTokensLegend {
        let mut legend = self
            .semantic_options()
            .map(|o| o.legend)
            .unwrap_or_default();
        for kind in &mut legend.token_types {
            *kind = highlight_name(kind.as_str()).to_owned().into();
        }
        legend
    }

    fn semantic_tokens(
        &self,
        text: &Rope,
        range: std::ops::Range<usize>,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<SemanticTokens>> {
        let this = self.clone();
        let text = text.clone();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |_| {
            let options = this
                .semantic_options()
                .ok_or_else(|| anyhow::anyhow!("semantic tokens unavailable"))?;
            let text = executor.spawn(async move { text.to_string() }).await;
            let snapshot = this.synchronized(&text, &executor).await?;
            let edits = this.edits();
            let mut params = json!({"textDocument":{"uri":this.uri}});
            let method = if full_tokens(&options) {
                "textDocument/semanticTokens/full"
            } else {
                let lines = Lines::new(&text);
                params["range"] = json!(Range::new(
                    lines.position(range.start, this.encoding())?,
                    lines.position(range.end, this.encoding())?,
                ));
                "textDocument/semanticTokens/range"
            };
            let response = this.client.request(method, params).await?;
            this.unchanged(&snapshot, edits)?;
            let tokens =
                serde_json::from_value::<Option<SemanticTokens>>(response)?.unwrap_or_default();
            let encoding = this.encoding();
            let tokens = executor
                .spawn(async move {
                    editor_tokens(tokens, &text, encoding, options.legend.token_types.len())
                })
                .await?;
            this.unchanged(&snapshot, edits)?;
            Ok(tokens)
        })
    }
}

/// Decode server deltas before converting columns, then encode the editor's UTF-32 positions.
fn editor_tokens(
    mut tokens: SemanticTokens,
    text: &str,
    encoding: Encoding,
    types: usize,
) -> Result<SemanticTokens> {
    let lines = Lines::new(text);
    let mut server = Position::default();
    let mut previous = Position::default();
    let mut previous_end = Position::default();
    let mut data = Vec::with_capacity(tokens.data.len());
    for token in tokens.data {
        server.line = server
            .line
            .checked_add(token.delta_line)
            .ok_or_else(|| anyhow::anyhow!("token line overflow"))?;
        server.character = (if token.delta_line == 0 {
            server.character
        } else {
            0
        })
        .checked_add(token.delta_start)
        .ok_or_else(|| anyhow::anyhow!("token column overflow"))?;
        let end = server
            .character
            .checked_add(token.length)
            .ok_or_else(|| anyhow::anyhow!("token length overflow"))?;
        if token.token_type as usize >= types || token.length == 0 {
            continue;
        }
        let range = lines.convert_range(
            Range::new(server, Position::new(server.line, end)),
            encoding,
            Encoding::Editor,
        )?;
        ensure!(
            range.start.line == range.end.line && range.start >= previous_end,
            "overlapping semantic tokens"
        );
        if range.start == range.end {
            continue;
        }
        data.push(SemanticToken {
            delta_line: range.start.line - previous.line,
            delta_start: if range.start.line == previous.line {
                range.start.character - previous.character
            } else {
                range.start.character
            },
            length: range.end.character - range.start.character,
            ..token
        });
        previous = range.start;
        previous_end = range.end;
    }
    tokens.data = data;
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(data: serde_json::Value) -> SemanticTokens {
        serde_json::from_value(json!({"data":data})).unwrap()
    }

    #[test]
    fn semantic_tokens_convert_unicode_deltas_and_line_endings() {
        let text = "😀 JSValue café\r\n.stringList\rsetObject";
        let expected = tokens(json!([
            0, 2, 7, 0, 0, 0, 8, 4, 1, 0, 1, 1, 10, 2, 0, 0, 11, 9, 3, 0
        ]));
        for (encoding, first, length) in [
            (Encoding::Utf8, 5, 5),
            (Encoding::Utf16, 3, 4),
            (Encoding::Utf32, 2, 4),
        ] {
            let input = tokens(json!([
                0, first, 7, 0, 0, 0, 8, length, 1, 0, 1, 1, 10, 2, 0, 1, 0, 9, 3, 0
            ]));
            assert_eq!(editor_tokens(input, text, encoding, 4).unwrap(), expected);
        }
    }

    #[test]
    fn semantic_tokens_validate_ranges_and_keep_deltas_after_unknown_types() {
        let input = tokens(json!([0, 0, 3, 99, 0, 0, 4, 3, 0, 0]));
        assert_eq!(
            editor_tokens(input, "one two", Encoding::Utf16, 1).unwrap(),
            tokens(json!([0, 4, 3, 0, 0]))
        );
        for (text, data) in [
            ("😀", json!([0, 1, 1, 0, 0])),
            ("abc", json!([0, 0, 2, 0, 0, 0, 1, 1, 0, 0])),
            ("abc", json!([0, 1, u32::MAX, 0, 0])),
            ("abc", json!([u32::MAX, 0, 1, 0, 0, 1, 0, 1, 0, 0])),
        ] {
            assert!(editor_tokens(tokens(data), text, Encoding::Utf16, 1).is_err());
        }
    }

    #[test]
    fn semantic_categories_resolve_in_both_stock_palettes() {
        use gpui_kit::component::highlighter::HighlightTheme;
        for theme in [
            HighlightTheme::default_light(),
            HighlightTheme::default_dark(),
        ] {
            for name in [
                "class",
                "struct",
                "typeParameter",
                "function",
                "method",
                "property",
                "enumMember",
                "parameter",
                "decorator",
                "regexp",
            ] {
                assert!(theme.style(highlight_name(name)).is_some(), "{name}");
            }
        }
        for (options, full) in [
            (json!({"full":true}), true),
            (json!({"full":{"delta":true}}), true),
            (json!({"full":false,"range":true}), false),
        ] {
            let mut options = options;
            options["legend"] = json!({"tokenTypes":[],"tokenModifiers":[]});
            assert_eq!(full_tokens(&serde_json::from_value(options).unwrap()), full);
        }
    }
}
