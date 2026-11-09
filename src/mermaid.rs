//! Mermaid fences rendered natively: merman lays out a resvg-safe SVG, resvg rasterizes it to a
//! PNG in the media cache, named by the source and theme, so a diagram renders once.
mod fences;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::text::{
    MarkdownNode, MarkdownParseContext, MarkdownPlugin, TextView, markdown_ast,
};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

#[derive(Clone)]
enum State {
    Pending,
    Ready(Diagram),
    Failed(String),
}

// shortcut: unbounded map of small entries, cap it if sessions hold thousands of diagrams.
static DIAGRAMS: LazyLock<Mutex<HashMap<String, State>>> = LazyLock::new(Mutex::default);

struct Images(Entity<crate::media::Lru>);
impl Global for Images {}

fn diagram(source: &str, dark: bool, cx: &mut App) -> State {
    let key = key(source, dark);
    let mut diagrams = DIAGRAMS.lock().unwrap();
    if let Some(state) = diagrams.get(&key)
        && !matches!(state, State::Ready(d) if !d.png.exists())
    {
        return state.clone();
    }
    diagrams.insert(key.clone(), State::Pending);
    drop(diagrams);
    let source = source.to_string();
    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async move { render(&source, dark) })
            .await;
        DIAGRAMS.lock().unwrap().insert(
            key,
            match result {
                Ok(diagram) => State::Ready(diagram),
                Err(error) => State::Failed(error),
            },
        );
        cx.update(|cx| cx.refresh_windows());
    })
    .detach();
    State::Pending
}

pub(crate) struct MermaidPlugin {
    pub(crate) group: &'static str,
}

impl MarkdownPlugin for MermaidPlugin {
    fn name(&self) -> &str {
        "mermaid"
    }
    fn is_block(&self) -> bool {
        true
    }

    fn parse(
        &self,
        node: &markdown_ast::Node,
        cx: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        let markdown_ast::Node::Code(code) = node else {
            return None;
        };
        if !code.lang.as_deref()?.eq_ignore_ascii_case("mermaid") {
            return None;
        }
        let source = cx.node_source(node)?;
        if !fences::closed(code, source) {
            return None;
        }
        let ch = source.chars().next()?;
        let len = source.chars().take_while(|c| *c == ch).count();
        // A standalone fence keeps the native fallback independent of list/quote prefixes.
        let fence = ch.to_string().repeat(len);
        let fallback = format!("{fence}mermaid\n{}\n{fence}", code.value);
        Some(
            MarkdownNode::new("mermaid", fallback)
                .text(code.value.clone())
                .markdown(source.to_string())
                .accessibility_label(format!("Mermaid diagram {}", code.value)),
        )
    }

    fn render(&self, node: &MarkdownNode, _: &mut Window, cx: &mut App) -> impl IntoElement {
        if !cx.has_global::<Images>() {
            let cache = crate::media::Lru::new(cx);
            cx.set_global(Images(cache));
        }
        let state = diagram(node.as_text(), cx.theme().is_dark(), cx);
        let id = node.source_range().map_or(0, |range| range.start);
        let group = self.group;
        let body = match state {
            State::Ready(d) => div()
                .w_full()
                .min_w_0()
                .relative()
                .px_4()
                .pt(px(28.))
                .pb(px(14.))
                .bg(cx.theme().muted)
                .border_1()
                .border_color(cx.theme().border)
                .rounded(cx.theme().semantic_tokens().radius.md)
                .child(div().absolute().top_2().right_2().child(header(
                    node.as_text().to_string().into(),
                    Some(d.clone()),
                    group,
                    cx,
                )))
                .child(
                    div()
                        .id("md-mermaid")
                        .role(Role::Image)
                        .aria_label(node.accessibility_name().to_string())
                        .test_support()
                        .relative()
                        .w_full()
                        .h(px(d.size.1.min(560.)))
                        .child(
                            img(d.png)
                                .image_cache(&cx.global::<Images>().0)
                                .absolute()
                                .inset_0()
                                .size_full()
                                .object_fit(ObjectFit::ScaleDown),
                        ),
                )
                .into_any_element(),
            state => {
                let fallback = crate::ui::code_blocks(
                    TextView::markdown("mermaid-source", node.data::<String>().unwrap().clone()),
                    group,
                )
                .code_block_actions(move |block, _, cx| header(block.code(), None, group, cx));
                v_flex()
                    .w_full()
                    .min_w_0()
                    .when_some(
                        match state {
                            State::Failed(e) => Some(e),
                            _ => None,
                        },
                        |d, e| {
                            d.child(
                                h_flex()
                                    .id("md-mermaid-error")
                                    .test_support()
                                    .gap_1()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(Icon::new(IconName::TriangleAlert).with_size(px(12.)))
                                    .child(format!("Mermaid diagram not rendered: {e}")),
                            )
                        },
                    )
                    .child(fallback)
                    .into_any_element()
            }
        };
        div()
            .id(("mermaid-block", id))
            .w_full()
            .min_w_0()
            .child(body)
    }
}

fn header(source: SharedString, diagram: Option<Diagram>, group: &'static str, cx: &App) -> Div {
    h_flex()
        .h(px(20.))
        .gap_1()
        .pl_1()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child("mermaid")
        .child(crate::ui::copy_button("copy", source, group))
        .child(
            Button::new("open-mermaid")
                .ghost()
                .xsmall()
                .label("Open")
                .disabled(diagram.is_none())
                .on_click(move |_, _, cx| {
                    if let Some(d) = &diagram
                        && let Ok(url) = url::Url::from_file_path(&d.png)
                    {
                        cx.open_url(url.as_str());
                    }
                }),
        )
}

/// Longest side of the rasterized diagram, in px (drawn at half size for Retina).
const MAX: f32 = 2400.;
/// Sources above this are shown as source, not laid out.
const MAX_SOURCE: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Diagram {
    pub(crate) png: PathBuf,
    /// Logical size in px.
    pub(crate) size: (f32, f32),
}

fn key(source: &str, dark: bool) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    ("merman-0.9.2", source, dark).hash(&mut h);
    format!("mermaid-{:016x}", h.finish())
}

/// Lays out and rasterizes one diagram. Blocking: background only.
pub(crate) fn render(source: &str, dark: bool) -> Result<Diagram, String> {
    if source.len() > MAX_SOURCE {
        return Err("diagram too large to render".into());
    }
    let dir = crate::media::cache_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let key = key(source, dark);
    let png = dir.join(format!("{key}.png"));
    if !png.exists() {
        rasterize(source, dark, &png)?;
    }
    // The PNG is twice the logical size, so the cap shrinks the logical size too.
    let size = imagesize::size(&png).map_err(|e| e.to_string())?;
    Ok(Diagram {
        png,
        size: (size.width as f32 / 2., size.height as f32 / 2.),
    })
}

fn rasterize(source: &str, dark: bool, png: &Path) -> Result<(), String> {
    let svg = svg(source, dark)?;
    let tree = resvg::usvg::Tree::from_data(svg.as_bytes(), &OPTIONS).map_err(|e| e.to_string())?;
    let (w, h) = (tree.size().width(), tree.size().height());
    // Twice the logical size for Retina, capped.
    let scale = (2f32).min(MAX / w.max(h));
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new((w * scale).ceil() as u32, (h * scale).ceil() as u32)
            .ok_or("diagram has no size")?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    crate::media::produce(png, |tmp| pixmap.save_png(tmp).map_err(|e| e.to_string()))
}

/// System fonts, so labels render as text rather than vanish.
static OPTIONS: LazyLock<resvg::usvg::Options<'static>> = LazyLock::new(|| {
    let mut options = resvg::usvg::Options::default();
    options.fontdb_mut().load_system_fonts();
    options
});

fn svg(source: &str, dark: bool) -> Result<String, String> {
    use merman::svg::{RenderResourcePolicy, SvgPipeline};
    use merman::{
        Engine, MermaidConfig, OperationControl, RenderOutput, RenderRequest, Renderer,
        SvgEnvironment, SvgRequest,
    };
    let config = MermaidConfig::from_value(serde_json::json!({
        "theme": if dark { "dark" } else { "default" },
        "fontFamily": "Helvetica, Arial, sans-serif",
    }));
    let renderer = Renderer::new().with_engine(Engine::new().with_site_config(config));
    let request = SvgRequest {
        environment: SvgEnvironment::deterministic()
            .with_resource_policy(RenderResourcePolicy::interactive()),
        pipeline: Some(SvgPipeline::resvg_safe()),
        ..SvgRequest::default()
    };
    let control = OperationControl::new().with_deadline(Duration::from_secs(5));
    match renderer.render(RenderRequest::svg(source, control, request)) {
        Ok(RenderOutput::Svg(Some(svg))) => Ok(svg.into_parts().0),
        Ok(_) => Err("no Mermaid diagram detected".into()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[cfg(feature = "test-support")]
    #[gpui_kit::test]
    fn missing_cached_mermaid_is_rendered_again(cx: &mut TestAppContext) {
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::state::TEST_HOME
            .get_or_init(|| std::env::temp_dir().join(format!("shark-ui-{}", std::process::id())));
        let source = "flowchart LR\n Pruned --> Restored";
        let key = key(source, false);
        DIAGRAMS.lock().unwrap().insert(
            key.clone(),
            State::Ready(Diagram {
                png: std::env::temp_dir()
                    .join(format!("shark-missing-mermaid-{}.png", std::process::id())),
                size: (100., 100.),
            }),
        );
        cx.update(|cx| assert!(matches!(diagram(source, false, cx), State::Pending)));
        cx.run_until_parked();
        let diagrams = DIAGRAMS.lock().unwrap();
        assert!(matches!(diagrams.get(&key), Some(State::Ready(d)) if d.png.exists()));
    }

    #[::core::prelude::v1::test]
    fn renders_and_caches_by_theme_and_reports_errors() {
        // UI tests clear the shared cache; the same directory they use keeps the real home clean.
        #[cfg(feature = "test-support")]
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::state::TEST_HOME
            .get_or_init(|| std::env::temp_dir().join(format!("shark-ui-{}", std::process::id())));
        let src = "flowchart TD\n  A[Start] --> B[Done]\n";
        let light = render(src, false).unwrap();
        assert!(light.png.exists());
        assert!(light.size.0 > 10. && light.size.1 > 10.);
        assert_eq!(render(src, false).unwrap(), light, "cached");
        let dark = render(src, true).unwrap();
        assert_ne!(dark.png, light.png, "the theme is part of the key");
        assert!(render("flowchart TD\n  A -->", false).is_err());
        assert!(render("not a diagram at all", false).is_err());
    }
}
