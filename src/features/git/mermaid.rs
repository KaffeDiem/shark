//! Mermaid fences rendered natively: merman lays out a resvg-safe SVG, resvg rasterizes it to a
//! PNG in the media cache, named by the source and theme, so a diagram renders once.
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

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

    #[::core::prelude::v1::test]
    fn renders_and_caches_by_theme_and_reports_errors() {
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
