//! Rich Git documents: Markdown in the house style, closed Mermaid fences as diagrams, and
//! standalone images or videos. Relative assets come from the document's captured revision,
//! never the working tree. A document is a list of blocks, so long ones are virtualized.
use super::mermaid::{self, Diagram};
use crate::media::{self, Kind, Lru, MediaFile, MediaView, Playing, Side, Source};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::TextViewState;
use gpui_kit::component::{ActiveTheme, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::PathBuf;
use std::rc::Rc;

/// Where a document's relative links point.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DocContext {
    /// The repository worktree, read only through Git objects.
    pub(crate) repo: PathBuf,
    /// The commit relative assets resolve against; `None` when unknown.
    pub(crate) rev: Option<String>,
    /// The document's directory inside the repository (`""` for the root).
    pub(crate) dir: String,
    /// `https://github.com/owner/repo`, for opening what can't be shown here.
    pub(crate) web: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Block {
    Text(String),
    Mermaid(String),
    Media { url: String, alt: String },
}

/// Text blocks are cut at blank lines once they reach this size, and at line ends past `HARD`,
/// so the reader lays out only what is on screen.
const CHUNK: usize = 2048;
const HARD: usize = 8192;

fn fence(line: &str) -> Option<(u8, usize, &str)> {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let ch = *trimmed.as_bytes().first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let len = trimmed.bytes().take_while(|b| *b == ch).count();
    let info = trimmed[len..].trim();
    (len >= 3 && !(ch == b'`' && info.contains('`'))).then_some((ch, len, info))
}

fn closes(line: &str, ch: u8, len: usize) -> bool {
    fence(line).is_some_and(|(c, l, info)| c == ch && l >= len && info.is_empty())
}

/// Splits Markdown into blocks. Fences stay whole; an unclosed fence runs to the end. With
/// `chunk`, long text is also cut into pieces.
pub(crate) fn blocks(source: &str, chunk: bool) -> Vec<Block> {
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    // Link reference definitions apply document-wide; each block gets a copy (they render
    // nothing), so `[text][1]` still resolves after a split.
    let mut refs = String::new();
    let mut in_fence: Option<(u8, usize)> = None;
    for line in &lines {
        match (in_fence, fence(line)) {
            (Some((ch, len)), _) if closes(line, ch, len) => in_fence = None,
            (Some(_), _) => {}
            (None, Some((ch, len, _))) => in_fence = Some((ch, len)),
            (None, None) if reference(line) => {
                refs.push_str(line.trim_end());
                refs.push('\n');
            }
            _ => {}
        }
    }
    let mut out = vec![];
    let mut text = String::new();
    let flush = |text: &mut String, out: &mut Vec<Block>| {
        if !text.trim().is_empty() {
            let mut block = std::mem::take(text);
            if !refs.is_empty() {
                block = format!("{}\n\n{refs}", block.trim_end());
            }
            out.push(Block::Text(block));
        }
        text.clear();
    };
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some((ch, len, info)) = fence(line) {
            let close = (i + 1..lines.len()).find(|&j| closes(lines[j], ch, len));
            let lang = info.split_whitespace().next().unwrap_or("");
            match close {
                Some(j) if lang.eq_ignore_ascii_case("mermaid") => {
                    flush(&mut text, &mut out);
                    out.push(Block::Mermaid(lines[i + 1..j].concat()));
                    i = j + 1;
                }
                Some(j) => {
                    text.push_str(&lines[i..=j].concat());
                    i = j + 1;
                }
                None => {
                    text.push_str(&lines[i..].concat());
                    break;
                }
            }
            continue;
        }
        if let Some(media) = media_line(line) {
            flush(&mut text, &mut out);
            out.extend(media);
            i += 1;
            continue;
        }
        // A line past HARD (one huge paragraph, minified text) is cut at spaces or char
        // boundaries, since a block is laid out whole.
        if chunk && line.len() > HARD {
            flush(&mut text, &mut out);
            let mut rest = line;
            while !rest.is_empty() {
                let at = rest.floor_char_boundary(HARD);
                let at = match rest[..at].rfind(' ') {
                    Some(sp) if at < rest.len() => sp + 1,
                    _ => at,
                };
                text.push_str(&rest[..at]);
                flush(&mut text, &mut out);
                rest = &rest[at..];
            }
            i += 1;
            continue;
        }
        text.push_str(line);
        if chunk && text.len() >= CHUNK {
            let next_plain = lines.get(i + 1).is_none_or(|n| !continues(n));
            // Past HARD, a paragraph may end at a line break, but tables, lists, quotes and
            // indented code stay whole.
            let hard = text.len() >= HARD && next_plain && !continues(line);
            if (line.trim().is_empty() && next_plain) || hard {
                flush(&mut text, &mut out);
            }
        }
        i += 1;
    }
    flush(&mut text, &mut out);
    out
}

/// Whether a line continues a structure that must not be split from the line before.
fn continues(line: &str) -> bool {
    let t = line.trim_start();
    line.starts_with([' ', '\t'])
        || t.starts_with(['|', '>', '-', '*', '+'])
        || t.split_once(['.', ')']).is_some_and(|(n, _)| {
            !n.is_empty() && n.len() < 10 && n.bytes().all(|b| b.is_ascii_digit())
        })
}

/// `[label]: destination`, but not a footnote, which renders visibly.
fn reference(line: &str) -> bool {
    let t = line.trim_start_matches(' ');
    line.len() - t.len() <= 3
        && t.starts_with('[')
        && !t.starts_with("[^")
        && t.split_once("]:")
            .is_some_and(|(l, d)| l.len() > 1 && !d.trim().is_empty())
}

/// An `https` URL to an image, video or GitHub attachment.
fn embeds(url: &str) -> bool {
    url.starts_with("https://")
        && !url.contains(char::is_whitespace)
        && (media::fetch::attachment(url) || media::fetch::url_kind(url).is_some())
}

/// Images on a line of their own: `![alt](url)`, `<img src="…">`, or a bare media or GitHub
/// attachment URL. `data:` images stay inline Markdown.
fn media_line(line: &str) -> Option<Vec<Block>> {
    if line.starts_with("    ") || line.starts_with('\t') {
        return None;
    }
    let mut rest = line.trim();
    if rest.is_empty() {
        return None;
    }
    // A Markdown link alone on its line, to an image, video or GitHub attachment.
    if let Some(r) = rest.strip_prefix('[')
        && !r.starts_with('!')
        && let Some((text, r)) = r.split_once("](")
        && let Some(url) = r.strip_suffix(')')
        && embeds(url)
    {
        return Some(vec![Block::Media {
            url: url.into(),
            alt: text.into(),
        }]);
    }
    let bare = rest.trim_start_matches('<').trim_end_matches('>');
    if embeds(bare) {
        return Some(vec![Block::Media {
            url: bare.into(),
            alt: String::new(),
        }]);
    }
    let mut out = vec![];
    while !rest.is_empty() {
        let (url, alt, after) = if let Some(r) = rest.strip_prefix("![") {
            let (alt, r) = r.split_once("](")?;
            let end = r.find(')')?;
            let target = r[..end].trim();
            let url = target.split_whitespace().next()?.trim_matches(['<', '>']);
            (url.to_string(), alt.to_string(), &r[end + 1..])
        } else if rest
            .get(..4)
            .is_some_and(|p| p.eq_ignore_ascii_case("<img"))
        {
            let end = rest.find('>')?;
            let tag = &rest[..end];
            (
                attr(tag, "src")?,
                attr(tag, "alt").unwrap_or_default(),
                &rest[end + 1..],
            )
        } else {
            return None;
        };
        if url.is_empty() || url.starts_with("data:") {
            return None;
        }
        out.push(Block::Media { url, alt });
        rest = after.trim_start();
    }
    Some(out)
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find(name) {
        let at = from + at;
        from = at + name.len();
        let before = lower[..at].chars().last();
        if !before.is_some_and(char::is_whitespace) {
            continue;
        }
        let r = tag[from..].trim_start().strip_prefix('=')?.trim_start();
        let q = r.chars().next()?;
        return Some(match q {
            '"' | '\'' => r[1..].split(q).next()?.to_string(),
            _ => r
                .split(|c: char| c.is_whitespace() || c == '>')
                .next()?
                .to_string(),
        });
    }
    None
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A repository path for a relative link, or `None` when it leaves the repository.
pub(crate) fn repo_path(dir: &str, url: &str) -> Option<String> {
    let url = decode(url.split(['?', '#']).next().unwrap_or(url));
    let joined = match url.strip_prefix('/') {
        Some(root) => root.to_string(),
        None if dir.is_empty() => url,
        None => format!("{dir}/{url}"),
    };
    let mut parts: Vec<&str> = vec![];
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// What one media block shows.
fn resolve(url: &str, ctx: Option<&DocContext>) -> Result<MediaFile, String> {
    if url.contains("://") || url.starts_with("//") {
        let fetched = media::fetch::fetch(url)?;
        return Ok(MediaFile {
            path: fetched.name.into(),
            kind: fetched.kind,
            old: None,
            new: Some(fetched.side),
        });
    }
    let ctx = ctx.ok_or("relative links need a repository")?;
    let path = repo_path(&ctx.dir, url).ok_or("the link leaves the repository")?;
    let kind = media::kind(&path).ok_or("not an image or video")?;
    let rev = ctx
        .rev
        .as_deref()
        .ok_or("the document's revision is unknown")?;
    let blob = crate::git::history::blob_at(&ctx.repo, rev, &path)
        .map_err(|_| "this revision isn't available locally".to_string())?
        .ok_or("not found at this revision")?;
    if blob.symlink() {
        return Err("a symbolic link; not followed".into());
    }
    Ok(MediaFile {
        path: path.into(),
        kind,
        old: None,
        new: Some(Side {
            sha: blob.id,
            repo: ctx.repo.clone(),
            source: Source::Blob,
        }),
    })
}

/// Where Open in browser goes for a block's URL.
fn web_link(url: &str, ctx: Option<&DocContext>) -> Option<String> {
    if url.starts_with("https://") || url.starts_with("http://") {
        return Some(url.into());
    }
    let ctx = ctx?;
    let path = repo_path(&ctx.dir, url)?;
    let rev = ctx.rev.as_deref().unwrap_or("HEAD");
    // Segments are percent-encoded: `a#b.png` must not become a fragment.
    let mut link = url::Url::parse(ctx.web.as_deref()?).ok()?;
    (link.path_segments_mut().ok()?)
        .pop_if_empty()
        .extend(["blob", rev])
        .extend(path.split('/'));
    Some(link.into())
}

/// What a block shows, made when it first renders.
enum View {
    /// A diagram being laid out or a file being fetched.
    Loading,
    Text(Entity<TextViewState>),
    Diagram(Diagram),
    /// A diagram that failed to render, with its source as a code fence.
    Unrendered(String, Entity<TextViewState>),
    Media(Entity<MediaView>),
    /// A file that can't be shown.
    Failed(String),
}

struct Item {
    block: Block,
    /// `None` until the block first renders.
    view: Option<View>,
}

/// The spinner box shown while a diagram renders or a file loads.
fn loading(cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .h(px(120.))
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme.radius)
        .bg(theme.muted.opacity(0.4))
        .child(Spinner::new().color(theme.muted_foreground))
}

pub(crate) struct RichDoc {
    source: String,
    ctx: Option<DocContext>,
    items: Vec<Item>,
    dark: Option<bool>,
    cache: Entity<Lru>,
    /// Virtualizes the blocks when the document is read in full.
    list: Option<ListState>,
    width: Rc<std::cell::Cell<Pixels>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<Playing> for RichDoc {}

impl RichDoc {
    pub(crate) fn new(
        source: String,
        ctx: Option<DocContext>,
        cache: Entity<Lru>,
        full: bool,
    ) -> RichDoc {
        let items: Vec<Item> = blocks(&source, full)
            .into_iter()
            .map(|block| Item { block, view: None })
            .collect();
        let list = full.then(|| ListState::new(items.len(), ListAlignment::Top, px(800.)));
        RichDoc {
            source,
            ctx,
            items,
            dark: None,
            cache,
            list,
            width: Rc::new(std::cell::Cell::new(px(0.))),
            _subs: vec![],
        }
    }

    pub(crate) fn source(&self) -> &str {
        &self.source
    }

    /// Stops any playing video, when its document leaves the screen.
    pub(crate) fn stop(&mut self, cx: &mut Context<Self>) {
        for item in &self.items {
            if let Some(View::Media(v)) = &item.view {
                v.update(cx, |v, cx| v.stop(cx));
            }
        }
    }

    fn start_diagram(&mut self, ix: usize, src: String, dark: bool, cx: &mut Context<Self>) {
        self.items[ix].view = Some(View::Loading);
        cx.spawn(async move |this, cx| {
            let out = cx
                .background_executor()
                .spawn({
                    let src = src.clone();
                    async move { mermaid::render(&src, dark) }
                })
                .await;
            this.update(cx, |s, cx| {
                // A theme change while rendering already started a newer one.
                if s.dark == Some(dark) {
                    s.items[ix].view = Some(match out {
                        Ok(d) => View::Diagram(d),
                        Err(e) => View::Unrendered(
                            e,
                            cx.new(|cx| TextViewState::markdown(&format!("```\n{src}```\n"), cx)),
                        ),
                    });
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn start_media(&mut self, ix: usize, url: String, alt: String, cx: &mut Context<Self>) {
        let ctx = self.ctx.clone();
        self.items[ix].view = Some(View::Loading);
        cx.spawn(async move |this, cx| {
            let out = cx
                .background_executor()
                .spawn(async move { resolve(&url, ctx.as_ref()) })
                .await;
            this.update(cx, |s, cx| {
                s.items[ix].view = Some(match out {
                    Ok(file) => {
                        let label = if alt.trim().is_empty() {
                            match file.kind {
                                Kind::Image => "Image".to_string(),
                                Kind::Video => "Video".to_string(),
                            }
                        } else {
                            alt
                        };
                        let (cache, width) = (s.cache.clone(), s.width.clone());
                        let view = cx.new(|_| MediaView::new(file, cache, width).only(1, label));
                        s._subs
                            .push(cx.subscribe(&view, |s, playing, _: &Playing, cx| {
                                for item in &s.items {
                                    if let Some(View::Media(v)) = &item.view
                                        && *v != playing
                                    {
                                        v.update(cx, |v, cx| v.stop(cx));
                                    }
                                }
                                cx.emit(Playing);
                            }));
                        View::Media(view)
                    }
                    Err(e) => View::Failed(e),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_block(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, faint, radius) = (
            theme.muted_foreground,
            theme.muted.opacity(0.4),
            theme.radius,
        );
        let el = match self.items[ix].block.clone() {
            Block::Text(text) => {
                let state = match &self.items[ix].view {
                    Some(View::Text(state)) => state.clone(),
                    _ => {
                        let state = cx.new(|cx| TextViewState::markdown(&text, cx));
                        self.items[ix].view = Some(View::Text(state.clone()));
                        state
                    }
                };
                crate::chat::markdown(&state, "git-row", vec![]).into_any_element()
            }
            Block::Mermaid(src) => match &self.items[ix].view {
                Some(View::Diagram(d)) => div()
                    .id(("git-mermaid", ix))
                    .test_support()
                    .relative()
                    .w_full()
                    .h(px(d.size.1.min(900.)))
                    .child(
                        img(d.png.clone())
                            .image_cache(&self.cache)
                            .absolute()
                            .inset_0()
                            .size_full()
                            .object_fit(ObjectFit::ScaleDown),
                    )
                    .into_any_element(),
                Some(View::Unrendered(e, source)) => v_flex()
                    .id(("git-mermaid-error", ix))
                    .test_support()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_1()
                            .text_xs()
                            .text_color(muted)
                            .child(Icon::new(IconName::TriangleAlert).with_size(px(12.)))
                            .child(format!("Mermaid diagram not rendered: {e}")),
                    )
                    .child(crate::chat::markdown(source, "git-row", vec![]))
                    .into_any_element(),
                Some(_) => loading(cx).into_any_element(),
                None => {
                    self.start_diagram(ix, src, cx.theme().is_dark(), cx);
                    loading(cx).into_any_element()
                }
            },
            Block::Media { url, alt } => {
                let link = web_link(&url, self.ctx.as_ref());
                let caption = h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(muted)
                    .child(div().flex_1().min_w_0().truncate().child(url.clone()))
                    .when_some(link, |d, link| {
                        d.child(
                            Button::new(("git-media-open", ix))
                                .label("Open in browser")
                                .icon(IconName::ExternalLink)
                                .ghost()
                                .xsmall()
                                .on_click(move |_, _, cx| cx.open_url(&link)),
                        )
                    });
                let body = match &self.items[ix].view {
                    Some(View::Media(view)) => view.clone().into_any_element(),
                    Some(View::Failed(e)) => div()
                        .id(("git-media-error", ix))
                        .test_support()
                        .w_full()
                        .p_3()
                        .rounded(radius)
                        .bg(faint)
                        .text_sm()
                        .text_color(muted)
                        .child(format!(
                            "Can't show {}: {e}",
                            if alt.trim().is_empty() {
                                "this file"
                            } else {
                                alt.as_str()
                            }
                        ))
                        .into_any_element(),
                    Some(_) => loading(cx).into_any_element(),
                    None => {
                        self.start_media(ix, url, alt, cx);
                        loading(cx).into_any_element()
                    }
                };
                v_flex()
                    .id(("git-media", ix))
                    .test_support()
                    .gap_1()
                    .child(body)
                    .child(caption)
                    .into_any_element()
            }
        };
        div().w_full().min_w_0().pb_2().child(el).into_any_element()
    }

    #[cfg(test)]
    pub(crate) fn rendered_len(&self, cx: &App) -> usize {
        self.items
            .iter()
            .filter_map(|i| match &i.view {
                Some(View::Text(t) | View::Unrendered(_, t)) => Some(t),
                _ => None,
            })
            .map(|t| t.read(cx).rendered_text().as_str().len())
            .sum()
    }

    #[cfg(test)]
    pub(crate) fn diagram_state(&self, ix: usize) -> Option<Result<PathBuf, String>> {
        match self.items[ix].view.as_ref()? {
            View::Diagram(d) => Some(Ok(d.png.clone())),
            View::Unrendered(e, _) => Some(Err(e.clone())),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn media_view(&self, ix: usize) -> Option<Entity<MediaView>> {
        match self.items.get(ix)?.view.as_ref()? {
            View::Media(v) => Some(v.clone()),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn media_error(&self, ix: usize) -> Option<String> {
        match self.items.get(ix)?.view.as_ref()? {
            View::Failed(e) => Some(e.clone()),
            _ => None,
        }
    }
}

impl Render for RichDoc {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dark = cx.theme().is_dark();
        if self.dark != Some(dark) {
            // Only diagrams follow the theme (text is styled when drawn); the cache keeps both.
            self.dark = Some(dark);
            for item in &mut self.items {
                if matches!(item.block, Block::Mermaid(_)) {
                    item.view = None;
                }
            }
        }
        match self.list.clone() {
            Some(list) => list_view(list, cx).into_any_element(),
            None => v_flex()
                .w_full()
                .min_w_0()
                .children((0..self.items.len()).map(|i| self.render_block(i, cx)))
                .into_any_element(),
        }
    }
}

fn list_view(state: ListState, cx: &mut Context<RichDoc>) -> impl IntoElement {
    list(
        state,
        cx.processor(|s, i, _, cx| {
            div()
                .w_full()
                .px_4()
                .child(
                    div()
                        .w_full()
                        .max_w(px(680.))
                        .mx_auto()
                        .child(s.render_block(i, cx)),
                )
                .into_any_element()
        }),
    )
    .size_full()
    .py_2()
}

impl RichDoc {
    pub(crate) fn list(&self) -> Option<&ListState> {
        self.list.as_ref()
    }

    pub(crate) fn scroll_by(&self, delta: Pixels) {
        if let Some(l) = &self.list {
            l.scroll_by(delta);
        }
    }

    pub(crate) fn scroll_to_start(&self) {
        if let Some(l) = &self.list {
            l.scroll_to(ListOffset::default());
        }
    }

    pub(crate) fn scroll_to_end(&self) {
        if let Some(l) = &self.list {
            l.scroll_to_end();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media(url: &str, alt: &str) -> Block {
        Block::Media {
            url: url.into(),
            alt: alt.into(),
        }
    }

    #[::core::prelude::v1::test]
    fn splits_fences_media_and_long_text() {
        let src = "Intro\n\n```mermaid\nflowchart TD\n A-->B\n```\n\n![shot](docs/a.png)\n\
                   <img width=\"300\" alt=\"demo\" src=\"https://github.com/user-attachments/assets/abc\">\n\
                   https://github.com/user-attachments/assets/def\n\
                   inline ![x](y.png) stays\n\
                   ```rust\n![not](media.png)\n```\n\
                   ![d](data:image/png;base64,AAAA)\n";
        let b = blocks(src, true);
        assert_eq!(b[0], Block::Text("Intro\n\n".into()));
        assert_eq!(b[1], Block::Mermaid("flowchart TD\n A-->B\n".into()));
        assert_eq!(b[2], media("docs/a.png", "shot"));
        assert_eq!(
            b[3],
            media("https://github.com/user-attachments/assets/abc", "demo")
        );
        assert_eq!(
            b[4],
            media("https://github.com/user-attachments/assets/def", "")
        );
        let Block::Text(rest) = &b[5] else {
            panic!("{b:?}")
        };
        assert!(rest.contains("inline ![x](y.png)") && rest.contains("![not](media.png)"));
        assert!(rest.contains("data:image"));
        assert_eq!(b.len(), 6);

        // Multibyte text never splits a character.
        for line in ["Hi 👋\n", "日本語で説明\n", "你好\n", "é<img\n"] {
            assert_eq!(blocks(line, true), [Block::Text(line.into())]);
        }
        assert_eq!(
            blocks(
                "[demo](https://github.com/user-attachments/assets/x)\n",
                true
            ),
            [media(
                "https://github.com/user-attachments/assets/x",
                "demo"
            )]
        );
        assert!(matches!(
            &blocks("[docs](https://x.test/a)\n", true)[0],
            Block::Text(_)
        ));

        let unclosed = blocks("a\n```mermaid\nflowchart TD\n", true);
        assert_eq!(
            unclosed.len(),
            1,
            "an unclosed fence stays source: {unclosed:?}"
        );

        let long = "word ".repeat(100) + "\n\n";
        let doc = long.repeat(50);
        let parts = blocks(&doc, true);
        assert!(parts.len() > 5, "long documents become several blocks");
        assert_eq!(blocks(&doc, false).len(), 1, "previews are not chunked");
        let joined: String = parts
            .iter()
            .map(|b| match b {
                Block::Text(t) => t.as_str(),
                _ => "",
            })
            .collect();
        assert_eq!(joined.trim_end(), doc.trim_end(), "nothing is lost");

        let table = format!("| a | b |\n|---|---|\n{}", "| cell | cell |\n".repeat(1000));
        let line = "wörd ".repeat(10_000) + "\n";
        let parts = blocks(&line, true);
        assert!(parts.len() > 5, "a newline-free paragraph is cut too");
        assert!(
            parts
                .iter()
                .all(|b| matches!(b, Block::Text(t) if t.len() <= HARD))
        );
        assert_eq!(blocks(&line, false).len(), 1);

        assert_eq!(
            blocks(&table, true).len(),
            1,
            "a long table stays one block"
        );
        let refs = format!(
            "See [one][1].\n\n{}\n\n[1]: https://x.test\n",
            "word ".repeat(600)
        );
        let parts = blocks(&refs, true);
        assert!(parts.len() > 1);
        assert!(matches!(&parts[0], Block::Text(t) if t.contains("[1]: https://x.test")));
    }

    #[::core::prelude::v1::test]
    fn relative_paths_stay_in_the_repository() {
        assert_eq!(
            repo_path("docs", "img/a%20b.png").as_deref(),
            Some("docs/img/a b.png")
        );
        assert_eq!(
            repo_path("docs", "../a.png?raw=1").as_deref(),
            Some("a.png")
        );
        assert_eq!(repo_path("docs", "/root.png").as_deref(), Some("root.png"));
        assert_eq!(repo_path("", "../../etc/passwd"), None);
        let ctx = DocContext {
            repo: "/r".into(),
            rev: Some("abc".into()),
            dir: "docs".into(),
            web: Some("https://github.com/o/r".into()),
        };
        assert_eq!(
            web_link("a.png", Some(&ctx)).as_deref(),
            Some("https://github.com/o/r/blob/abc/docs/a.png")
        );
        assert_eq!(
            web_link("a%23b%20c.png", Some(&ctx)).as_deref(),
            Some("https://github.com/o/r/blob/abc/docs/a%23b%20c.png")
        );
        assert_eq!(
            attr("<img alt='x y' src=a.png>", "src").as_deref(),
            Some("a.png")
        );
        assert_eq!(
            attr("<img data-src=\"no\" src=\"yes\">", "src").as_deref(),
            Some("yes")
        );
    }
}
