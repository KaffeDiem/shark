//! The preview pane: one `.context` file, drawn by type.
use crate::editor::language;
use crate::features::git::commits::{markdown, text_of};
use crate::features::git::document::{DocContext, RichDoc, on_disk};
use crate::media::{self, Lru, MediaView, human_bytes};
use crate::ui::{faint, toolbar};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::SystemTime;

/// Text above this is not read for a preview.
const MAX_TEXT: u64 = 1024 * 1024;
/// Images and video above this are not prepared for a preview.
const MAX_MEDIA: u64 = 50 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Kind {
    Plan,
    Transcript,
    Image,
    Video,
    Markdown,
    Text,
    Pdf,
    File,
}

impl Kind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Kind::Plan => "Plan",
            Kind::Transcript => "Transcript",
            Kind::Image => "Image",
            Kind::Video => "Video",
            Kind::Markdown => "Markdown",
            Kind::Text => "Text",
            Kind::Pdf => "PDF",
            Kind::File => "File",
        }
    }
}

/// The kind of a path inside `.context` (`plans/a.md`, `attachments/x transcript.md`).
pub(crate) fn kind(path: &str) -> Kind {
    let name = path.rsplit('/').next().unwrap_or(path);
    let lower = name.to_ascii_lowercase();
    if markdown(path) {
        // Handoff writes `<chat title> transcript.md`, and ` transcript (1).md` on a repeat.
        let handoff = path.starts_with("attachments/") && lower.contains(" transcript");
        return match () {
            _ if path.starts_with("plans/") => Kind::Plan,
            _ if handoff => Kind::Transcript,
            _ => Kind::Markdown,
        };
    }
    match media::kind(path) {
        Some(media::Kind::Image) => return Kind::Image,
        Some(media::Kind::Video) => return Kind::Video,
        None => {}
    }
    let ext = lower.rsplit_once('.').map_or("", |(_, e)| e);
    match ext {
        "pdf" => Kind::Pdf,
        "txt" | "text" | "log" | "json" | "jsonl" | "yaml" | "yml" | "toml" | "csv" | "tsv"
        | "xml" | "html" | "css" | "js" | "ts" | "tsx" | "jsx" | "rs" | "py" | "rb" | "go"
        | "swift" | "sh" | "zsh" | "c" | "h" | "cpp" | "java" | "kt" | "sql" | "diff" | "patch"
        | "ini" | "env" | "conf" => Kind::Text,
        _ => Kind::File,
    }
}

/// Whether a file of this kind and size is too big to preview.
fn too_large(kind: Kind, size: u64) -> bool {
    match kind {
        Kind::Image | Kind::Video => size > MAX_MEDIA,
        Kind::Pdf => false,
        _ => size > MAX_TEXT,
    }
}

/// The `.context` folder, given a file's path inside it and its absolute path.
fn context_root(rel: &str, abs: &Path) -> PathBuf {
    abs.ancestors()
        .nth(rel.split('/').count())
        .unwrap_or(abs)
        .to_path_buf()
}

pub(crate) enum PreviewEvent {
    Attach,
    Open,
    ShowInChat,
    TogglePin,
    /// The "More" menu, at the click.
    More(Point<Pixels>),
}

enum Loaded {
    Unsupported(u64),
    Text { text: String, markdown: bool },
    Media(media::MediaFile),
}

pub(crate) enum Content {
    Empty,
    Loading,
    Failed(String),
    Unsupported(u64),
    Text(Entity<EditorState>),
    Doc(Entity<RichDoc>),
    Media(Entity<MediaView>),
}

/// Reads one file. Blocking: background only.
fn load(rel: &str, abs: &Path) -> (Option<SystemTime>, Result<Loaded, String>) {
    let meta = std::fs::metadata(abs);
    let mtime = meta.as_ref().ok().and_then(|m| m.modified().ok());
    // The time is kept on failure too, so an unchanged broken file isn't retried.
    (
        mtime,
        meta.map_err(|e| e.to_string())
            .and_then(|m| read(rel, abs, m.len())),
    )
}

fn read(rel: &str, abs: &Path, size: u64) -> Result<Loaded, String> {
    let k = kind(rel);
    let loaded = match k {
        // Too large and unsupported share a screen; `state` tells them apart by size.
        _ if too_large(k, size) => Loaded::Unsupported(size),
        Kind::Pdf => Loaded::Unsupported(size),
        Kind::Image | Kind::Video => {
            let ctx = doc_context(rel, abs);
            Loaded::Media(on_disk(
                &ctx,
                rel.into(),
                media::kind(rel).ok_or("not media")?,
            )?)
        }
        _ => match text_of(std::fs::read(abs).map_err(|e| e.to_string())?) {
            Some(text) => Loaded::Text {
                text,
                markdown: markdown(rel),
            },
            None => Loaded::Unsupported(size),
        },
    };
    Ok(loaded)
}

/// Relative links in a file resolve inside `.context`.
fn doc_context(rel: &str, abs: &Path) -> DocContext {
    DocContext {
        repo: context_root(rel, abs),
        rev: None,
        dir: rel.rsplit_once('/').map_or("", |(d, _)| d).to_string(),
        web: None,
        on_disk: true,
    }
}

pub(crate) struct Preview {
    file: Option<(String, PathBuf)>,
    mtime: Option<SystemTime>,
    removed: bool,
    /// The chat (id, title) whose approved plan this file copies.
    plan_chat: Option<(String, String)>,
    /// The shown file is pinned, for the toolbar button.
    pinned: bool,
    pub(crate) generation: u64,
    pub(crate) content: Content,
    cache: Entity<Lru>,
}

impl EventEmitter<PreviewEvent> for Preview {}

impl Preview {
    pub(crate) fn new(cache: Entity<Lru>) -> Self {
        Self {
            file: None,
            mtime: None,
            removed: false,
            plan_chat: None,
            pinned: false,
            generation: 0,
            content: Content::Empty,
            cache,
        }
    }

    pub(crate) fn set_plan_chat(&mut self, chat: Option<(String, String)>, cx: &mut Context<Self>) {
        if self.plan_chat != chat {
            self.plan_chat = chat;
            cx.notify();
        }
    }

    pub(crate) fn set_pinned(&mut self, pinned: bool, cx: &mut Context<Self>) {
        if self.pinned != pinned {
            self.pinned = pinned;
            cx.notify();
        }
    }

    /// Stops any video or document video, when the file changes or the pane goes away.
    pub(crate) fn stop(&mut self, cx: &mut Context<Self>) {
        match &self.content {
            Content::Media(v) => v.update(cx, |v, cx| v.stop(cx)),
            Content::Doc(d) => d.update(cx, |d, cx| d.stop(cx)),
            _ => {}
        }
    }

    pub(crate) fn show(
        &mut self,
        file: Option<(String, PathBuf)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.stop(cx);
        self.removed = false;
        self.mtime = None;
        self.file = file;
        self.generation += 1;
        self.content = match self.file {
            Some(_) => Content::Loading,
            None => Content::Empty,
        };
        self.start(window, cx);
        cx.notify();
    }

    /// Re-reads the file in place when its modification time moved.
    pub(crate) fn reload_if_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, abs)) = &self.file else { return };
        // The first load has no time yet: a poll would restart it, and a slow one never ends.
        if matches!(self.content, Content::Loading) {
            return;
        }
        let now = std::fs::metadata(abs).and_then(|m| m.modified()).ok();
        if !self.removed && now.is_some() && now != self.mtime {
            self.generation += 1;
            self.start(window, cx);
        }
    }

    /// The file left `.context`: what is shown stays, the toolbar says so.
    pub(crate) fn mark_removed(&mut self, cx: &mut Context<Self>) {
        // A load in flight still lands: the file was readable when it started.
        self.removed = true;
        cx.notify();
    }

    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((rel, abs)) = self.file.clone() else {
            return;
        };
        let generation = self.generation;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { load(&rel, &abs) })
                .await;
            this.update_in(cx, |s, window, cx| {
                if s.generation == generation {
                    s.apply(result, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Builds the views, here where a window is at hand.
    fn apply(
        &mut self,
        (mtime, result): (Option<SystemTime>, Result<Loaded, String>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((rel, abs)) = self.file.clone() else {
            return;
        };
        self.mtime = mtime;
        let loaded = match result {
            Ok(r) => r,
            Err(e) => {
                self.content = Content::Failed(e);
                return cx.notify();
            }
        };
        // A rewrite lands at the old scroll position.
        let doc_scroll = match &self.content {
            Content::Doc(d) => d.read(cx).list().map(|l| l.logical_scroll_top()),
            _ => None,
        };
        let editor_scroll = match &self.content {
            Content::Text(i) => Some(i.read(cx).scroll_offset()),
            _ => None,
        };
        self.stop(cx);
        let content = match loaded {
            Loaded::Unsupported(n) => Content::Unsupported(n),
            Loaded::Media(mf) => {
                let (cache, name) = (self.cache.clone(), rel.rsplit('/').next().unwrap_or(&rel));
                let name = name.to_string();
                let width = Rc::new(std::cell::Cell::new(px(0.)));
                Content::Media(cx.new(|_| MediaView::new(mf, cache, width).only(1, name)))
            }
            Loaded::Text {
                text,
                markdown: true,
            } => {
                let doc = RichDoc::new(
                    text,
                    Some(doc_context(&rel, &abs)),
                    self.cache.clone(),
                    true,
                );
                if let (Some(top), Some(list)) = (doc_scroll, doc.list()) {
                    let last = list.item_count().saturating_sub(1);
                    list.scroll_to(ListOffset {
                        item_ix: top.item_ix.min(last),
                        ..top
                    });
                }
                Content::Doc(cx.new(|_| doc))
            }
            Loaded::Text { text, .. } => match &self.content {
                // A rewrite keeps the editor, and with it the scroll position.
                Content::Text(input) => {
                    input.update(cx, |i, cx| {
                        i.set_value(text, window, cx);
                        // `set_value` scrolls to the top; the offset is clamped on layout.
                        if let Some(at) = editor_scroll {
                            i.set_scroll_offset(at, cx);
                        }
                    });
                    Content::Text(input.clone())
                }
                _ => {
                    let lang = language(&rel);
                    let input = cx.new(|cx| {
                        EditorState::new(window, cx)
                            .default_value(text)
                            .language(lang)
                            .soft_wrap(true)
                    });
                    input.update(cx, |i, cx| i.set_readonly(true, cx));
                    Content::Text(input)
                }
            },
        };
        self.content = content;
        cx.notify();
    }

    fn render_toolbar(&self, rel: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (name, folder) = rel.rsplit_once('/').map_or((rel, ""), |(d, n)| (n, d));
        let folder = format!(".context/{folder}")
            .trim_end_matches('/')
            .to_string();
        h_flex()
            .px_4()
            .py_2()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(name.to_string()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(theme.sidebar_foreground)
                            .child(folder),
                    )
                    .when_some(self.plan_chat.as_ref(), |d, (_, title)| {
                        d.child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("Copy of the approved plan in {title}.")),
                        )
                    })
                    .when(self.removed, |d| {
                        d.child(
                            div()
                                .id("artifact-removed")
                                .test_support()
                                .text_xs()
                                .text_color(theme.warning_foreground)
                                .child("Removed from .context"),
                        )
                    }),
            )
            .when(self.plan_chat.is_some(), |d| {
                d.child(
                    Button::new("artifact-show-chat")
                        .label("Show in chat")
                        .small()
                        .ghost()
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(PreviewEvent::ShowInChat)))
                        .map(|b| toolbar("artifact-show-chat", b)),
                )
            })
            .child(
                Button::new("artifact-attach")
                    .label("Attach to chat")
                    .small()
                    .ghost()
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(PreviewEvent::Attach)))
                    .map(|b| toolbar("artifact-attach", b)),
            )
            .child({
                let name = if self.pinned { "Unpin" } else { "Pin" };
                Button::new("artifact-pin")
                    .icon(if self.pinned {
                        IconName::PinOff
                    } else {
                        IconName::Pin
                    })
                    .accessibility_label(name)
                    .tooltip(name)
                    .small()
                    .ghost()
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(PreviewEvent::TogglePin)))
                    .map(|b| toolbar("artifact-pin", b))
            })
            .child(
                Button::new("artifact-open")
                    .icon(IconName::SquarePen)
                    .accessibility_label("Open in editor")
                    .tooltip("Open in editor")
                    .small()
                    .ghost()
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(PreviewEvent::Open)))
                    .map(|b| toolbar("artifact-open", b)),
            )
            .child(
                Button::new("artifact-more")
                    .icon(IconName::Ellipsis)
                    .accessibility_label("More actions")
                    .tooltip("More actions")
                    .small()
                    .ghost()
                    .on_click(cx.listener(|_, e: &ClickEvent, _, cx| {
                        cx.emit(PreviewEvent::More(e.position()))
                    }))
                    .map(|b| toolbar("artifact-more", b)),
            )
    }

    /// A centered message, with buttons under it.
    fn state(&self, rel: &str, abs: &Path, size: Option<u64>, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let (muted, faint) = (theme.muted_foreground, faint(theme));
        let name = rel.rsplit('/').next().unwrap_or(rel).to_string();
        let button = |id: &'static str, label: &'static str| Button::new(id).label(label).small();
        let (open, reveal) = (abs.to_path_buf(), abs.to_path_buf());
        let message = |text: String| {
            div()
                .id("artifact-state")
                .test_support()
                .aria_label(text.clone())
                .text_sm()
                .text_color(muted)
                .child(text)
        };
        let col = v_flex().size_full().items_center().justify_center().gap_2();
        let open_in = |id: &'static str| {
            let open = open.clone();
            toolbar(
                id,
                button(id, "Open in…").on_click(move |_, _, cx| cx.open_with_system(&open)),
            )
        };
        match size {
            // Too large: the size is all there is to say.
            Some(n) if too_large(kind(rel), n) => col
                .child(message(format!(
                    "Too large to preview ({}).",
                    human_bytes(n)
                )))
                .child(
                    h_flex()
                        .gap_2()
                        .child(toolbar(
                            "artifact-too-large-open",
                            button("artifact-too-large-open", "Open in editor")
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(PreviewEvent::Open))),
                        ))
                        .child(open_in("artifact-too-large-open-in")),
                ),
            _ => col
                .child(match crate::file_icon::file_icon(&name, theme.is_dark()) {
                    Some(i) => img(i).size(px(32.)).flex_none().into_any_element(),
                    None => Icon::new(IconName::File)
                        .with_size(px(32.))
                        .text_color(faint)
                        .into_any_element(),
                })
                .child(div().text_base().child(name))
                .child(div().text_xs().text_color(muted).child(format!(
                    "{} · {}",
                    kind(rel).label(),
                    size.map(human_bytes).unwrap_or_default()
                )))
                .child(message("No preview for this file type.".into()))
                .child(
                    h_flex()
                        .gap_2()
                        .child(open_in("artifact-open-in"))
                        .child(toolbar(
                            "artifact-reveal",
                            button("artifact-reveal", "Reveal in Finder")
                                .on_click(move |_, _, cx| cx.reveal_path(&reveal)),
                        )),
                ),
        }
    }
}

impl Render for Preview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some((rel, abs)) = self.file.clone() else {
            return div().id("artifacts-preview").size_full().into_any_element();
        };
        let muted = cx.theme().muted_foreground;
        let body = match &self.content {
            Content::Empty => div().into_any_element(),
            Content::Loading => h_flex()
                .p_4()
                .gap_2()
                .text_sm()
                .text_color(muted)
                .child(Spinner::new().color(muted))
                .child("Reading…")
                .into_any_element(),

            Content::Failed(e) => div()
                .p_4()
                .text_sm()
                .text_color(muted)
                .child(format!("Could not read this file: {e}"))
                .into_any_element(),

            Content::Unsupported(n) => self.state(&rel, &abs, Some(*n), cx).into_any_element(),

            Content::Text(input) => div()
                .size_full()
                .child(Editor::new(input).readonly(true).appearance(false).h_full())
                .into_any_element(),

            Content::Doc(doc) => div()
                .size_full()
                .child(
                    div()
                        .h_full()
                        .w_full()
                        .max_w(px(680.))
                        .mx_auto()
                        .child(doc.clone()),
                )
                .into_any_element(),

            Content::Media(v) => div()
                .id("artifact-media")
                .p_4()
                .size_full()
                .overflow_y_scroll()
                .child(v.clone())
                .into_any_element(),
        };
        v_flex()
            .id("artifacts-preview")
            .test_support()
            .size_full()
            .min_h_0()
            .child(self.render_toolbar(&rel, cx))
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn classifies_by_place_and_extension() {
        let k = kind;
        assert_eq!(k("plans/a.md"), Kind::Plan);
        assert_eq!(k("attachments/Fix bug transcript.md"), Kind::Transcript);
        assert_eq!(k("attachments/Fix bug transcript (1).md"), Kind::Transcript);
        assert_eq!(k("attachments/notes.md"), Kind::Markdown);
        assert_eq!(k("scratch/Transcript.md"), Kind::Markdown);
        assert_eq!(k("shot.PNG"), Kind::Image);
        assert_eq!(k("a/b.mov"), Kind::Video);
        assert_eq!(k("x.json"), Kind::Text);
        assert_eq!(k("x.PDF"), Kind::Pdf);
        assert_eq!(k("x.bin"), Kind::File);
        assert_eq!(k("Makefile"), Kind::File);
        assert_eq!(Kind::Pdf.label(), "PDF");
    }

    #[::core::prelude::v1::test]
    fn size_limits_follow_the_kind() {
        assert!(!too_large(Kind::Text, MAX_TEXT));
        assert!(too_large(Kind::Markdown, MAX_TEXT + 1));
        assert!(!too_large(Kind::Image, MAX_TEXT + 1));
        assert!(too_large(Kind::Image, MAX_MEDIA + 1));
        assert!(!too_large(Kind::Pdf, u64::MAX));
    }

    #[::core::prelude::v1::test]
    fn binary_and_root() {
        assert_eq!(text_of(b"hi".to_vec()).as_deref(), Some("hi"));
        assert_eq!(text_of(b"a\0b".to_vec()), None);
        assert_eq!(text_of(vec![0xff, 0xfe]), None);
        let abs = Path::new("/t/.context/plans/a.md");
        assert_eq!(context_root("plans/a.md", abs), Path::new("/t/.context"));
        assert_eq!(
            context_root("a.md", Path::new("/t/.context/a.md")),
            Path::new("/t/.context")
        );
    }
}
