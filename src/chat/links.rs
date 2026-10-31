//! Markdown and link handling for chat text.
use super::*;

/// A copy button whose icon turns into a check for 2s. Until `group` is hovered it is
/// invisible but laid out, so showing it never moves anything.
pub(crate) fn copy_button(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    group: &'static str,
) -> Div {
    div()
        .flex_none()
        .invisible()
        .group_hover(group, |s| s.visible())
        .child(Clipboard::new(id).value(text).tooltip("Copy"))
}

/// Where `cwd`-relative chat links may point: the workspace dir, then each repo's worktree.
pub(crate) fn link_roots(ws: &Workspace) -> Vec<PathBuf> {
    let dir = ws.dir();
    let repos = ws.repos.iter().map(|r| dir.join(&r.name));
    std::iter::once(dir.clone()).chain(repos).collect()
}

/// A file URL for `path`; Foundation escapes spaces and Unicode.
fn file_url(path: &str) -> Option<String> {
    use objc2_foundation::{NSString, NSURL};
    let url = NSURL::fileURLWithPath(&NSString::from_str(path)).absoluteString();
    url.map(|u| u.to_string())
}

/// `s` with `%XX` escapes decoded; `None` when it has none or they are not valid UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let (b, mut out, mut i) = (s.as_bytes(), Vec::new(), 0);
    while i < b.len() {
        let hex = |k: usize| b.get(k).and_then(|c| (*c as char).to_digit(16));
        match (b[i], hex(i + 1), hex(i + 2)) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (c, ..) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok().filter(|d| d != s)
}

/// What a clicked link opens. URLs pass through; paths (absolute, `~/`, or relative to one
/// of `roots`, with any `:N[:M]` or `#L..` location dropped) become file URLs.
fn link_target(url: &str, roots: &[PathBuf]) -> Option<String> {
    if url.contains("://") || url.starts_with("mailto:") || url.starts_with("tel:") {
        return Some(url.to_string());
    }
    let mut stripped = url;
    if let Some((p, frag)) = url.rsplit_once('#')
        && frag
            .strip_prefix('L')
            .is_some_and(|d| d.starts_with(|c: char| c.is_ascii_digit()))
    {
        stripped = p;
    }
    for _ in 0..2 {
        match stripped.rsplit_once(':') {
            Some((p, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => stripped = p,
            _ => break,
        }
    }
    // A filename may itself contain ':' or '#', so the unstripped form is the fallback.
    let expand = |p: &str| match p.strip_prefix("~/") {
        Some(rest) => crate::state::home().join(rest),
        None => PathBuf::from(p),
    };
    let first = expand(stripped);
    // Links are often percent-encoded (`a%20b.txt`); try the decoded path first.
    for p in [
        percent_decode(stripped).map(|d| expand(&d)),
        Some(first.clone()),
        Some(expand(url)),
    ]
    .into_iter()
    .flatten()
    {
        if p.is_absolute() {
            if p.exists() {
                return file_url(&p.to_string_lossy());
            }
        } else if let Some(hit) = roots.iter().map(|r| r.join(&p)).find(|c| c.exists()) {
            // `components` drops a leading `./`.
            return file_url(&hit.components().collect::<PathBuf>().to_string_lossy());
        }
    }
    if first.is_absolute() {
        return file_url(&first.to_string_lossy());
    }
    Some(if url.starts_with("www.") {
        format!("https://{url}")
    } else {
        url.to_string()
    })
}

/// Markdown whose code blocks get the language and a copy button in their top-right corner.
/// The blocks keep the top padding the language label had, so the corner never covers code.
/// Relative file links resolve against `roots`.
pub(crate) fn markdown(
    md: &Entity<TextViewState>,
    group: &'static str,
    roots: Vec<PathBuf>,
) -> TextView {
    TextView::new(md)
        .selectable(true)
        .on_link_click(move |url, event, _, cx| {
            if !event.standard_click() && !event.is_middle_click() {
                return;
            }
            if let Some(target) = link_target(url, &roots) {
                cx.open_url(&target);
            }
        })
        .style(TextViewStyle::default().code_block(StyleRefinement::default().pt(px(28.))))
        .code_block_actions(move |block, _, cx| {
            h_flex()
                .h(px(20.))
                .gap_1()
                .pl_1()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .when_some(block.lang().filter(|l| !l.is_empty()), |d, l| d.child(l))
                .child(copy_button("copy", block.code(), group))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin

    /// A workspace dir holding `repo/src/lib.rs`, `repo/README.md`, and `repo/a b å.txt`.
    fn link_fixture(tag: &str) -> (PathBuf, Vec<PathBuf>) {
        let ws = std::env::temp_dir().join(format!("shark-links-{tag}-{}", std::process::id()));
        let repo = ws.join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        for f in ["src/lib.rs", "README.md", "a b å.txt", "x:1.txt"] {
            std::fs::write(repo.join(f), "").unwrap();
        }
        (ws.clone(), vec![ws, repo])
    }

    #[test]
    fn link_target_resolves_relative_paths() {
        let (ws, roots) = link_fixture("rel");
        let at = |rel: &str| Some(format!("file://{}/{rel}", ws.display()));
        // Prefixed by the repo name, resolved from the workspace dir.
        assert_eq!(
            link_target("repo/src/lib.rs", &roots),
            at("repo/src/lib.rs")
        );
        // Relative to the repo worktree.
        assert_eq!(link_target("src/lib.rs", &roots), at("repo/src/lib.rs"));
        for url in [
            "src/lib.rs:42",
            "src/lib.rs:42:7",
            "src/lib.rs#L10",
            "src/lib.rs#L10-L20",
            "src/lib.rs#L10C3",
        ] {
            assert_eq!(link_target(url, &roots), at("repo/src/lib.rs"), "{url}");
        }
        assert_eq!(link_target("./README.md#L10", &roots), at("repo/README.md"));
        assert_eq!(
            link_target("./repo/README.md", &roots),
            at("repo/README.md")
        );
        // Foundation escapes spaces and Unicode.
        assert_eq!(
            link_target("a b å.txt", &roots),
            at("repo/a%20b%20a%CC%8A.txt")
        );
        // Percent escapes decode before the lookup, with and without a location suffix.
        for url in [
            "a%20b%20%C3%A5.txt",
            "./a%20b%20a%CC%8A.txt:3",
            "a%20b å.txt#L2",
        ] {
            assert_eq!(
                link_target(url, &roots),
                at("repo/a%20b%20a%CC%8A.txt"),
                "{url}"
            );
        }
        std::fs::create_dir_all(ws.join("docs")).unwrap();
        std::fs::write(ws.join("docs/caf\u{e9} 1.txt"), "").unwrap();
        assert_eq!(
            link_target("docs/caf%C3%A9%201.txt", &roots),
            at("docs/cafe%CC%81%201.txt")
        );
        // A literal '%' in the name still resolves.
        std::fs::write(ws.join("100%.txt"), "").unwrap();
        assert_eq!(link_target("100%.txt", &roots), at("100%25.txt"));
        // A ':' that is part of the filename still resolves.
        assert_eq!(link_target("x:1.txt", &roots), at("repo/x:1.txt"));
    }

    #[test]
    fn link_target_passes_through_and_falls_back() {
        let (ws, roots) = link_fixture("pass");
        for url in [
            "https://example.com/a:1#L2",
            "http://example.com",
            "file:///tmp/a%20b.png",
            "mailto:a@b.c",
            "tel:+4512345678",
            "nope/missing.rs",
            "missing.rs:3",
            "#section",
        ] {
            assert_eq!(link_target(url, &roots).as_deref(), Some(url), "{url}");
        }
        assert_eq!(
            link_target("www.example.com/x", &roots).as_deref(),
            Some("https://www.example.com/x")
        );
        // Without roots a relative path has nowhere to resolve.
        assert_eq!(
            link_target("src/lib.rs", &[]).as_deref(),
            Some("src/lib.rs")
        );
        // Absolute paths become file URLs, existing or not.
        let abs = ws.join("repo/src/lib.rs");
        assert_eq!(
            link_target(&format!("{}:42", abs.display()), &roots),
            Some(format!("file://{}", abs.display()))
        );
        assert_eq!(
            link_target("/tmp/shark-missing/a b.png", &roots).as_deref(),
            Some("file:///tmp/shark-missing/a%20b.png")
        );
    }

    #[cfg(feature = "test-support")]
    #[gpui_kit::test]
    fn markdown_links_open_local_previews_and_web_urls(cx: &mut TestAppContext) {
        struct Links(Entity<TextViewState>, Vec<PathBuf>);
        impl Render for Links {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                markdown(&self.0, "links", self.1.clone())
            }
        }
        cx.update(gpui_kit::init);
        let ws = std::env::temp_dir().join(format!("shark-link-ui-{}", std::process::id()));
        std::fs::create_dir_all(ws.join("api/src")).unwrap();
        std::fs::write(ws.join("api/src/lib.rs"), "").unwrap();
        let roots = vec![ws.clone(), ws.join("api")];
        let lib = format!("file://{}/api/src/lib.rs", ws.display());
        for (target, expected) in [
            ("api/src/lib.rs", lib.as_str()),
            ("src/lib.rs:42", lib.as_str()),
            (
                "/tmp/shark-better-bg-render/activity-dark-hover.png",
                "file:///tmp/shark-better-bg-render/activity-dark-hover.png",
            ),
            (
                "/tmp/shark-better-bg-render/activity-light-hover.png",
                "file:///tmp/shark-better-bg-render/activity-light-hover.png",
            ),
            (
                "/tmp/preview blå #1.png",
                "file:///tmp/preview%20bla%CC%8A%20%231.png",
            ),
            (
                "https://example.com/preview.png",
                "https://example.com/preview.png",
            ),
            (
                "file:///tmp/preview%20one.png",
                "file:///tmp/preview%20one.png",
            ),
        ] {
            let (_, view) = cx.add_window_view(|_, cx| {
                Links(
                    cx.new(|cx| TextViewState::markdown(&format!("[Preview](<{target}>)"), cx)),
                    roots.clone(),
                )
            });
            let view: &mut VisualTestContext = view;
            let before = view.opened_url();
            view.simulate_mouse_down(
                point(px(10.), px(10.)),
                MouseButton::Right,
                Modifiers::default(),
            );
            view.simulate_mouse_up(
                point(px(10.), px(10.)),
                MouseButton::Right,
                Modifiers::default(),
            );
            assert_eq!(
                view.opened_url(),
                before,
                "right-click must not open {target}"
            );
            view.simulate_click(point(px(10.), px(10.)), Modifiers::default());
            assert_eq!(view.opened_url().as_deref(), Some(expected), "{target}");
        }
    }
}
