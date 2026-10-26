//! Highlight names from syntect for the diff's presentation. gpui-component keeps its syntect
//! backend `pub(crate)` (`SyntaxHighlighter::capture_names`, `highlighter::language_name`), so
//! this is a copy of the parts the diff needs, from `src/highlighter/syntect_backend.rs` and
//! `language_name.rs` of gpui-component 0.8.0 (Apache-2.0, see `mod.rs`).

use std::{collections::HashMap, ops::Range, sync::LazyLock};

use gpui_kit::SharedString;
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

/// Deserializing the syntax set takes tens of milliseconds, so it happens once,
/// on the first highlight, and is shared by every highlighter after that.
static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);

/// Highlight names, ordered and non-overlapping.
type Highlights = Vec<(Range<usize>, &'static str)>;

/// Names highlights for the presentation, which resolves styles later.
pub(crate) struct SyntaxHighlighter {
    language: SharedString,
}

impl SyntaxHighlighter {
    pub(crate) fn new(language: impl AsRef<str>) -> Self {
        Self {
            language: language.as_ref().to_owned().into(),
        }
    }

    /// Highlight names for `text`.
    pub(crate) fn capture_names(&mut self, text: &str) -> Vec<(Range<usize>, SharedString)> {
        let Some(syntax) = find_syntax(&self.language) else {
            return Vec::new();
        };
        parse(syntax, text)
            .into_iter()
            .map(|(range, name)| (range, SharedString::new_static(name)))
            .collect()
    }
}

/// The syntax for a code fence or editor language name.
///
/// Syntect matches a token against extensions and syntax names. Most of the
/// names the rest of the library uses are already one of those; the few that
/// are not are spelled here the way syntect knows them.
fn find_syntax(language: &str) -> Option<&'static SyntaxReference> {
    let syntaxes = &*SYNTAXES;
    let canonical = language_name(language);
    let alias = match canonical.as_ref() {
        "text" => return None,
        "csharp" => "cs",
        "make" => "Makefile",
        "markdown_inline" => "md",
        "embedded_template" | "ejs" => "erb",
        other => other,
    };
    [language, canonical.as_ref(), alias]
        .into_iter()
        .find_map(|token| syntaxes.find_syntax_by_token(token))
        .filter(|syntax| syntax.name != "Plain Text")
}

/// Runs syntect over `text` and keeps the spans that map to a highlight name.
fn parse(syntax: &SyntaxReference, text: &str) -> Highlights {
    let syntaxes = &*SYNTAXES;
    let mut parser = ParseState::new(syntax);
    let mut scopes = ScopeStack::new();
    let mut names = HashMap::<Scope, Option<&'static str>>::new();
    let mut highlights = Highlights::new();
    let mut offset = 0;

    let mut push = |range: Range<usize>, scopes: &ScopeStack, highlights: &mut Highlights| {
        if range.is_empty() {
            return;
        }
        // The innermost scope that names something wins, so the quotes of a
        // string — `punctuation.definition.string` inside `string` — color as
        // the string rather than as punctuation.
        let name = scopes
            .scopes
            .iter()
            .rev()
            .find_map(|scope| *names.entry(*scope).or_insert_with(|| semantic_name(*scope)));
        if let Some(name) = name {
            match highlights.last_mut() {
                Some((last, last_name)) if last.end == range.start && *last_name == name => {
                    last.end = range.end;
                }
                _ => highlights.push((range, name)),
            }
        }
    };

    for line in LinesWithEndings::from(text) {
        // A line syntect cannot parse keeps the scopes it started with; the
        // next line starts over from them rather than losing the document.
        if let Ok(operations) = parser.parse_line(line, syntaxes) {
            let mut cursor = 0;
            for (index, operation) in operations {
                push(offset + cursor..offset + index, &scopes, &mut highlights);
                let _ = scopes.apply(&operation);
                cursor = index;
            }
            push(
                offset + cursor..offset + line.len(),
                &scopes,
                &mut highlights,
            );
        }
        offset += line.len();
    }
    highlights
}

/// The theme's highlight name for a TextMate scope, or `None` to let an
/// enclosing scope decide.
fn semantic_name(scope: Scope) -> Option<&'static str> {
    let scope = scope.build_string();
    let is = |prefix: &str| {
        scope == prefix
            || scope
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('.'))
    };

    let name = if is("punctuation.definition.string")
        || is("punctuation.definition.comment")
        || is("invalid")
        || is("meta")
        || is("source")
        || is("text")
    {
        // Delimiters belong to what they delimit; the rest are containers
        // with no color of their own.
        return None;
    } else if is("comment.block.documentation") || is("comment.line.documentation") {
        "comment.doc"
    } else if is("comment") {
        "comment"
    } else if is("constant.character.escape") {
        "string.escape"
    } else if is("string.regexp") {
        "string.regex"
    } else if is("string") {
        "string"
    } else if is("constant.numeric") {
        "number"
    } else if is("constant.language.boolean") {
        "boolean"
    } else if is("constant") || is("support.constant") || is("entity.name.constant") {
        "constant"
    } else if is("keyword.operator") {
        "operator"
    } else if is("keyword.control.import") || is("meta.preprocessor") {
        "preproc"
    } else if is("keyword") || is("storage.modifier") {
        "keyword"
    } else if is("storage.type") {
        // `fn`, `class` and `let` are declarations, `int` and `bool` are types.
        if [
            "class",
            "enum",
            "function",
            "impl",
            "interface",
            "module",
            "namespace",
            "struct",
            "trait",
            "type",
            "let",
            "var",
            "const",
        ]
        .iter()
        .any(|word| scope.contains(word))
        {
            "keyword"
        } else {
            "type"
        }
    } else if is("entity.name.function")
        || is("support.function")
        || is("variable.function")
        || is("meta.function-call")
    {
        "function"
    } else if is("entity.name.type")
        || is("entity.name.class")
        || is("entity.name.struct")
        || is("entity.name.enum")
        || is("entity.name.trait")
        || is("entity.name.interface")
        || is("entity.other.inherited-class")
        || is("support.type")
        || is("support.class")
    {
        "type"
    } else if is("entity.name.tag") {
        "tag"
    } else if is("entity.other.attribute-name") {
        "attribute"
    } else if is("entity.name.label") {
        "label"
    } else if is("entity.name.section") || is("markup.heading") {
        "title"
    } else if is("variable.language") {
        "variable.special"
    } else if is("variable.other.member")
        || is("variable.other.property")
        || is("support.variable.property")
        || is("meta.mapping.key")
    {
        "property"
    } else if is("variable") {
        "variable"
    } else if is("markup.bold") {
        "emphasis.strong"
    } else if is("markup.italic") {
        "emphasis"
    } else if is("markup.underline.link") {
        "link_uri"
    } else if is("markup.raw") {
        "text.literal"
    } else if is("markup.list") {
        "punctuation.list_marker"
    } else if is("punctuation.section") || is("punctuation.definition.block") {
        "punctuation.bracket"
    } else if is("punctuation.separator") || is("punctuation.terminator") {
        "punctuation.delimiter"
    } else if is("punctuation") {
        "punctuation"
    } else {
        return None;
    };
    Some(name)
}

/// Built-in aliases for editing defaults when no exact grammar name is registered.
/// Availability of a parser does not change what a language name means.
pub(crate) fn language_name(name: &str) -> SharedString {
    let name = name.to_lowercase();
    match name.as_str() {
        "plain" => "text".into(),
        "plaintext" => "text".into(),
        "jsonc" => "json".into(),
        "sh" => "bash".into(),
        "c++" => "cpp".into(),
        "cs" => "csharp".into(),
        "scss" => "css".into(),
        "ex" => "elixir".into(),
        "js" => "javascript".into(),
        "kt" => "kotlin".into(),
        "kts" => "kotlin".into(),
        "ktm" => "kotlin".into(),
        "makefile" => "make".into(),
        "md" => "markdown".into(),
        "mdx" => "markdown".into(),
        "markdown-inline" => "markdown_inline".into(),
        "php3" => "php".into(),
        "php4" => "php".into(),
        "php5" => "php".into(),
        "phtml" => "php".into(),
        "protobuf" => "proto".into(),
        "py" => "python".into(),
        "rb" => "ruby".into(),
        "rs" => "rust".into(),
        "ts" => "typescript".into(),
        "yml" => "yaml".into(),
        "pyi" => "python".into(),
        _ => name.into(),
    }
}
