//! Tool calls in the transcript: their summaries and one-line input.
use super::*;

/// claude's tool for submitting a plan.
pub(crate) const PLAN_TOOL: &str = "ExitPlanMode";
/// claude's tool for asking the user questions.
pub(crate) const ASK_TOOL: &str = "AskUserQuestion";

pub(super) struct Call {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) summary: String,
    /// The full input, one `key: value` per line and capped, for the row's tooltip.
    pub(super) input: String,
    pub(super) result: Option<SharedString>,
    pub(super) is_error: bool,
    pub(super) open: bool,
}

pub(super) fn standalone(name: &str) -> Option<IconName> {
    match name {
        "Skill" => Some(IconName::BookOpen),
        "Agent" | "Task" => Some(IconName::Bot),
        // A submitted plan: its row opens the version in the plan pane.
        PLAN_TOOL => Some(IconName::ClipboardList),
        // The agent's questions: the row shows the answers.
        ASK_TOOL => Some(IconName::MessageCircleQuestionMark),
        _ => None,
    }
}

/// "Read 3 files, ran 2 commands" for a tool group, in first-use order.
pub(super) fn group_summary(calls: &[Call]) -> String {
    let mut parts: Vec<(&str, &str, &str, usize)> = vec![];
    for c in calls {
        let (verb, one, many) = match c.name.as_str() {
            "Read" => ("read", "file", "files"),
            "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => ("edited", "file", "files"),
            "Bash" => ("ran", "command", "commands"),
            "Grep" | "Glob" => ("searched", "pattern", "patterns"),
            "WebSearch" | "WebFetch" => ("looked up", "page", "pages"),
            _ => ("used", "tool", "tools"),
        };
        match parts.iter_mut().find(|p| p.0 == verb) {
            Some(p) => p.3 += 1,
            None => parts.push((verb, one, many, 1)),
        }
    }
    let s = parts
        .iter()
        .map(|&(v, one, many, n)| format!("{v} {n} {}", if n == 1 { one } else { many }))
        .collect::<Vec<_>>()
        .join(", ");
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

pub(super) fn summarize(input: &Value, root: &str) -> String {
    let field = [
        "skill",
        "command",
        "file_path",
        "pattern",
        "path",
        "url",
        "description",
        "prompt",
    ]
    .iter()
    .find_map(|k| input[k].as_str());
    let s = field
        .map(str::to_string)
        .unwrap_or_else(|| input.to_string());
    let s = s.lines().next().unwrap_or_default();
    // Paths inside the workspace read better as repo/file.
    let s = s
        .strip_prefix(root)
        .map_or(s, |r| r.trim_start_matches('/'));
    s.chars().take(120).collect()
}

/// The first `n` chars, with "…" when cut.
pub(super) fn cap(s: &str, n: usize) -> String {
    let mut c: String = s.chars().take(n).collect();
    if c.len() < s.len() {
        c.push('…');
    }
    c
}

pub(super) fn full_input(input: &Value) -> String {
    let s = match input.as_object() {
        Some(o) => (o.iter())
            .map(|(k, v)| {
                format!(
                    "{k}: {}",
                    v.as_str().map_or_else(|| v.to_string(), Into::into)
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        None => serde_json::to_string_pretty(input).unwrap_or_default(),
    };
    cap(&s, 2000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin

    #[test]
    fn summarizes_tool_groups() {
        let call = |name: &str, err| Call {
            id: String::new(),
            name: name.into(),
            summary: String::new(),
            input: String::new(),
            result: Some("".into()),
            is_error: err,
            open: false,
        };
        let calls = ["Read", "Bash", "Read", "Edit", "Write", "mcp__x__y"].map(|n| call(n, false));
        assert_eq!(
            group_summary(&calls),
            "Read 2 files, ran 1 command, edited 2 files, used 1 tool"
        );
        assert_eq!(group_summary(&[]), "");
        let input = serde_json::json!({"file_path": "/ws/feat/api/src/a.rs"});
        assert_eq!(summarize(&input, "/ws/feat"), "api/src/a.rs");
        let skill = serde_json::json!({"skill": "pdf", "args": "x"});
        assert_eq!(summarize(&skill, "/ws/feat"), "pdf");
        let bash = serde_json::json!({"command": "a\nb", "timeout": 5});
        assert_eq!(full_input(&bash), "command: a\nb\ntimeout: 5");
        assert_eq!(cap("abcdef", 3), "abc…");
    }
}
