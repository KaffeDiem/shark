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
    let mut chars = text.chars().peekable();
    let mut source = Position::default();
    let mut editor = Position::default();
    // Token endpoints are ordered, so each character is consumed at most once.
    let mut position = |target: Position| -> Result<Position> {
        ensure!(source <= target, "overlapping semantic tokens");
        while source < target {
            let Some(&c) = chars.peek() else { break };
            let crlf = c == '\r' && chars.clone().nth(1) == Some('\n');
            let newline = c == '\n' || (c == '\r' && (encoding != Encoding::Editor || crlf));
            if newline && source.line == target.line {
                break; // LSP columns past the line end clamp to the line end.
            }
            let width = match encoding {
                Encoding::Utf8 => c.len_utf8() as u32,
                Encoding::Utf16 => c.len_utf16() as u32,
                Encoding::Utf32 | Encoding::Editor => 1,
            };
            if !newline && source.line == target.line {
                ensure!(
                    width <= target.character - source.character,
                    "position splits a Unicode character"
                );
            }
            chars.next();
            if newline {
                source.line = source
                    .line
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("token line overflow"))?;
                source.character = 0;
            } else {
                source.character = source
                    .character
                    .checked_add(width)
                    .ok_or_else(|| anyhow::anyhow!("token column overflow"))?;
            }
            if c == '\n' || (c == '\r' && chars.peek() == Some(&'\n')) {
                if c == '\r' {
                    chars.next();
                }
                editor.line = editor
                    .line
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("editor line overflow"))?;
                editor.character = 0;
            } else {
                editor.character = editor
                    .character
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("editor column overflow"))?;
            }
        }
        Ok(editor)
    };
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
        let range = Range::new(
            position(server)?,
            position(Position::new(server.line, end))?,
        );
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
    fn semantic_token_cursor_matches_document_coordinates() {
        let text = "😀 café\r\nbare\rreturn\n";
        let lines = Lines::new(text);
        for encoding in [
            Encoding::Utf8,
            Encoding::Utf16,
            Encoding::Utf32,
            Encoding::Editor,
        ] {
            for line in 0..5 {
                for column in 0..16 {
                    let input = tokens(json!([line, column, 1, 0, 0]));
                    let range =
                        Range::new(Position::new(line, column), Position::new(line, column + 1));
                    let expected = lines.convert_range(range, encoding, Encoding::Editor);
                    let actual = editor_tokens(input, text, encoding, 1);
                    match expected {
                        Err(_) => assert!(actual.is_err()),
                        Ok(range) if range.start == range.end => {
                            assert!(actual.unwrap().data.is_empty())
                        }
                        Ok(range) => assert_eq!(
                            actual.unwrap(),
                            tokens(json!([
                                range.start.line,
                                range.start.character,
                                range.end.character - range.start.character,
                                0,
                                0
                            ]))
                        ),
                    }
                }
            }
        }
    }

    #[test]
    fn semantic_tokens_scan_long_lines_and_clamp_endpoints() {
        let text = "😀x ".repeat(20_000);
        for (encoding, width) in [
            (Encoding::Utf8, 6),
            (Encoding::Utf16, 4),
            (Encoding::Utf32, 3),
        ] {
            let input = SemanticTokens {
                result_id: None,
                data: (0..20_000)
                    .map(|i| SemanticToken {
                        delta_line: 0,
                        delta_start: if i == 0 { 0 } else { width },
                        length: width - 1,
                        token_type: 0,
                        token_modifiers_bitset: 0,
                    })
                    .collect(),
            };
            let output = editor_tokens(input, &text, encoding, 1).unwrap();
            assert_eq!(output.data.len(), 20_000);
            for (i, token) in output.data.iter().enumerate() {
                assert_eq!(token.delta_start, if i == 0 { 0 } else { 3 });
                assert_eq!(token.length, 2);
            }
        }
        let input = tokens(json!([0, 0, 99, 0, 0, 1, 0, 99, 0, 0, 10, 0, 1, 0, 0]));
        assert_eq!(
            editor_tokens(input, "a\r\nb", Encoding::Utf16, 1).unwrap(),
            tokens(json!([0, 0, 1, 0, 0, 1, 0, 1, 0, 0]))
        );
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
