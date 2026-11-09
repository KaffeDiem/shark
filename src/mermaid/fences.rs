use gpui_kit::component::text::markdown_ast;

pub(super) fn closed(code: &markdown_ast::Code, source: &str) -> bool {
    let Some(position) = &code.position else {
        return false;
    };
    let Some(ch @ (b'`' | b'~')) = source.bytes().next() else {
        return false;
    };
    let opening = source.bytes().take_while(|b| *b == ch).count();
    let closing = source
        .trim_end_matches([' ', '\t'])
        .bytes()
        .rev()
        .take_while(|b| *b == ch)
        .count();
    let value = code.value.as_bytes();
    let lines = usize::from(!value.is_empty())
        + value
            .iter()
            .enumerate()
            .filter(|(i, b)| **b == b'\n' || (**b == b'\r' && value.get(i + 1) != Some(&b'\n')))
            .count();
    // A fence-like suffix inside code counts as content; a real closing line does not.
    opening >= 3
        && closing >= opening
        && position.end.line.saturating_sub(position.start.line) > lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_fences_match_parser_positions_and_content() {
        // Values and end lines come from markdown-rs 1.0.0, including container termination.
        for (source, value, end_line, expected) in [
            ("```mermaid", "", 1, false),
            ("```mermaid\n", "", 2, false),
            ("```mermaid\n\n", "", 3, false),
            ("```mermaid\nx", "x", 2, false),
            ("```mermaid\nx\n", "x", 3, false),
            ("```mermaid\nx\n\n", "x\n", 4, false),
            ("```mermaid\nx\nabc```", "x\nabc```", 3, false),
            ("```mermaid\nx\n> ```", "x\n> ```", 3, false),
            ("```mermaid\n> x\n> abc```", "x\nabc```", 3, false),
            ("```mermaid\n> x", "x", 2, false),
            ("```mermaid\n```", "", 2, true),
            ("```mermaid\n\n```", "", 3, true),
            ("```mermaid\n\n\n```", "\n", 4, true),
            ("```mermaid\nx\n```", "x", 3, true),
            ("```Mermaid\r\nx\r\n```", "x", 3, true),
            ("```mermaid\nx\r```", "x", 3, true),
            ("```mermaid\nx\n````   ", "x", 3, true),
            ("~~~mermaid\nx\n~~~~\t", "x", 3, true),
            ("```mermaid\n> x\n> ```", "x", 3, true),
            ("```mermaid\n  x\n  ```", "x", 3, true),
            ("```mermaid\nx\n\n```", "x\n", 4, true),
            ("```mermaid\nx\n~~~", "x\n~~~", 3, false),
            ("````mermaid\nx\n```", "x\n```", 3, false),
        ] {
            let code = markdown_ast::Code {
                value: value.into(),
                position: Some(
                    serde_json::from_value(serde_json::json!({
                        "start": {"line": 1, "column": 1, "offset": 0},
                        "end": {"line": end_line, "column": 1, "offset": source.len()}
                    }))
                    .unwrap(),
                ),
                lang: Some("mermaid".into()),
                meta: None,
            };
            assert_eq!(closed(&code, source), expected, "{source:?}");
        }
    }
}
