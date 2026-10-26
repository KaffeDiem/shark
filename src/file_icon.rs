//! Colored file-type and folder icons: Material Icon Theme SVGs vendored in `assets/file-icons/`.
//! The one full-color icon set (docs/design.md, Icons); everything else is Lucide.
use gpui_kit::{Image, ImageFormat};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

macro_rules! svgs {
    ($($n:literal)*) => {
        [$(($n, include_bytes!(concat!("../assets/file-icons/", $n, ".svg")).as_slice())),*]
    };
}

// A `<name>_light` file is used in light mode; add one when a glyph fades on white.
const SVGS: &[(&str, &[u8])] = &svgs!(
    "c" "console" "cpp" "csharp" "css" "dart" "database" "docker" "elixir" "folder-android"
    "folder-api" "folder-app" "folder-benchmark" "folder-ci" "folder-claude" "folder-client"
    "folder-command" "folder-components" "folder-config" "folder-css" "folder-database"
    "folder-dist" "folder-docs" "folder-examples" "folder-font" "folder-git" "folder-github"
    "folder-hook" "folder-images" "folder-include" "folder-ios" "folder-lib" "folder-log"
    "folder-migrations" "folder-mock" "folder-node" "folder-packages" "folder-public"
    "folder-resource" "folder-routes" "folder-scripts" "folder-server" "folder-src" "folder-target"
    "folder-temp" "folder-test" "folder-tools" "folder-ui" "folder-utils" "folder-views"
    "folder-vscode" "git" "go" "go-mod" "graphql" "haskell" "html" "image" "java" "javascript"
    "json" "kotlin" "license" "lock" "lua" "makefile" "markdown" "nix" "npm" "php" "proto" "python"
    "react" "react_ts" "readme" "ruby" "rust" "sass" "scala" "settings" "svg" "swift" "test-js"
    "test-ts" "toml" "toml_light" "tune" "typescript" "typescript-def" "xml" "yaml" "zig"
);

/// The icon for a file name, or `None` for a type without one (callers show Lucide `File`).
pub fn file_icon(name: &str, dark: bool) -> Option<Arc<Image>> {
    icon(kind(name)?, dark)
}

/// The icon for a folder name (`src`, `tests/`, `.github`), or `None` (callers show `Folder`).
pub fn folder_icon(name: &str, dark: bool) -> Option<Arc<Image>> {
    icon(folder_kind(name)?, dark)
}

fn icon(kind: &str, dark: bool) -> Option<Arc<Image>> {
    static ICONS: OnceLock<HashMap<&str, Arc<Image>>> = OnceLock::new();
    let icons = ICONS.get_or_init(|| {
        (SVGS.iter())
            .map(|&(n, svg)| {
                (
                    n,
                    Arc::new(Image::from_bytes(ImageFormat::Svg, svg.to_vec())),
                )
            })
            .collect()
    });
    let light = (!dark).then(|| icons.get(format!("{kind}_light").as_str()));
    light.flatten().or_else(|| icons.get(kind)).cloned()
}

/// Folder names as Material Icon Theme matches them, `.github` and `__tests__` included.
fn folder_kind(name: &str) -> Option<&'static str> {
    let name = name.trim_end_matches('/').to_lowercase();
    Some(
        match name.trim_start_matches(['.', '_']).trim_end_matches('_') {
            "android" => "folder-android",
            "api" | "apis" | "restapi" => "folder-api",
            "app" | "apps" | "application" | "applications" => "folder-app",
            "benchmark" | "benchmarks" | "bench" | "benches" | "performance" | "perf"
            | "profiling" | "measure" | "measures" | "measurement" => "folder-benchmark",
            "ci" => "folder-ci",
            "claude" => "folder-claude",
            "client" | "clients" | "frontend" | "frontends" | "pwa" | "spa" => "folder-client",
            "command" | "commands" | "commandline" | "cmd" | "cli" | "clis" => "folder-command",
            "components" | "widget" | "widgets" | "fragments" => "folder-components",
            "cfg" | "cfgs" | "conf" | "confs" | "config" | "configs" | "configuration"
            | "configurations" | "setting" | "settings" | "meta-inf" | "option" | "options"
            | "pref" | "prefs" | "preference" | "preferences" | "props" | "properties" => {
                "folder-config"
            }
            "css" | "stylesheet" | "stylesheets" | "style" | "styles" => "folder-css",
            "db" | "data" | "database" | "databases" | "sql" | "storage" => "folder-database",
            "dist" | "out" | "output" | "outputs" | "build" | "builds" | "release" | "bin"
            | "distribution" | "built" | "compiled" => "folder-dist",
            "doc" | "docs" | "document" | "documents" | "documentation" | "post" | "posts"
            | "article" | "articles" | "wiki" | "news" | "blog" | "knowledge" | "diary"
            | "note" | "notes" => "folder-docs",
            "demo" | "demos" | "example" | "examples" | "sample" | "samples" | "sample-data" => {
                "folder-examples"
            }
            "font" | "fonts" | "typeface" | "typefaces" => "folder-font",
            "git" | "patches" | "githooks" | "submodules" => "folder-git",
            "github" => "folder-github",
            "hook" | "hooks" => "folder-hook",
            "images" | "image" | "imgs" | "img" | "icons" | "icon" | "icos" | "ico" | "figures"
            | "figure" | "figs" | "fig" | "screenshot" | "screenshots" | "screengrab"
            | "screengrabs" | "pic" | "pics" | "picture" | "pictures" | "photo" | "photos"
            | "photograph" | "photographs" | "texture" | "textures" => "folder-images",
            "inc" | "include" | "includes" | "partial" | "partials" | "inc64" => "folder-include",
            "ios" => "folder-ios",
            "lib" | "libs" | "library" | "libraries" | "vendor" | "vendors" | "thirdparty"
            | "third-party" | "lib64" | "external" | "externals" | "crates" => "folder-lib",
            "log" | "logs" | "logging" => "folder-log",
            "migrations" | "migration" => "folder-migrations",
            "draft" | "drafts" | "mock" | "mocks" | "fixture" | "fixtures" | "concept"
            | "concepts" | "sketch" | "sketches" => "folder-mock",
            "node" | "nodejs" | "node_modules" => "folder-node",
            "package" | "packages" | "pkg" | "pkgs" | "serverpackages" | "devpackages"
            | "dependencies" => "folder-packages",
            "public" | "www" | "wwwroot" | "web" | "website" | "websites" | "site" | "browser"
            | "browsers" | "proxy" => "folder-public",
            "resource" | "resources" | "res" | "asset" | "assets" | "static" | "report"
            | "reports" => "folder-resource",
            "routes" | "router" | "routers" | "navigation" | "navigations" | "routing" => {
                "folder-routes"
            }
            "script" | "scripts" | "scripting" | "xtask" => "folder-scripts",
            "server" | "servers" | "backend" | "backends" | "inventory" | "inventories"
            | "infrastructure" | "infra" => "folder-server",
            "src" | "srcs" | "source" | "sources" | "code" => "folder-src",
            "target" => "folder-target",
            "temp" | "tmp" | "cached" | "cache" => "folder-temp",
            "test" | "tests" | "testing" | "snapshots" | "spec" | "specs" | "testfiles" => {
                "folder-test"
            }
            "tools" | "toolkit" | "toolkits" | "toolbox" | "toolboxes" | "tooling" | "devtools"
            | "kit" | "kits" => "folder-tools",
            "presentation" | "gui" | "ui" | "ux" => "folder-ui",
            "util" | "utils" | "utility" | "utilities" => "folder-utils",
            "view" | "views" | "screen" | "screens" | "page" | "pages" | "public_html" | "html" => {
                "folder-views"
            }
            "vscode" | "vscode-test" => "folder-vscode",
            _ => return None,
        },
    )
}

fn kind(name: &str) -> Option<&'static str> {
    let name = name.to_lowercase();
    let by_name = match name.as_str() {
        "cargo.toml" | "cargo.lock" => "rust",
        "package.json" | "package-lock.json" | ".npmrc" | ".npmignore" => "npm",
        "go.mod" | "go.sum" | "go.work" | "go.work.sum" => "go-mod",
        "package.swift" | "package.resolved" => "swift",
        "dockerfile"
        | ".dockerignore"
        | "compose.yml"
        | "compose.yaml"
        | "docker-compose.yml"
        | "docker-compose.yaml" => "docker",
        "makefile" | "gnumakefile" => "makefile",
        "readme" | "readme.md" | "readme.txt" => "readme",
        "license" | "license.md" | "license.txt" | "copying" => "license",
        ".gitignore" | ".gitattributes" | ".gitmodules" | ".gitkeep" => "git",
        ".env" => "tune",
        _ if name.starts_with(".env.") => "tune",
        _ if name.starts_with("dockerfile.") => "docker",
        _ if name.ends_with(".d.ts") => "typescript-def",
        _ if [".test.ts", ".spec.ts", ".test.tsx", ".spec.tsx"]
            .iter()
            .any(|s| name.ends_with(s)) =>
        {
            "test-ts"
        }
        _ if [".test.js", ".spec.js", ".test.jsx", ".spec.jsx"]
            .iter()
            .any(|s| name.ends_with(s)) =>
        {
            "test-js"
        }
        _ => "",
    };
    if !by_name.is_empty() {
        return Some(by_name);
    }
    Some(match crate::editor::language(&name).as_str() {
        "rs" => "rust",
        "go" => "go",
        "swift" => "swift",
        "ts" | "mts" | "cts" => "typescript",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "react",
        "tsx" => "react_ts",
        "py" | "pyi" => "python",
        "rb" => "ruby",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "cs" => "csharp",
        "php" => "php",
        "lua" => "lua",
        "zig" => "zig",
        "ex" | "exs" => "elixir",
        "hs" => "haskell",
        "scala" | "sc" => "scala",
        "dart" => "dart",
        "sh" | "bash" | "zsh" | "fish" => "console",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" | "sass" => "sass",
        "json" | "jsonc" | "json5" | "jsonl" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "xml" => "xml",
        "md" | "markdown" | "mdx" => "markdown",
        "svg" => "svg",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "ico" | "icns" | "bmp" | "tiff" | "avif" => {
            "image"
        }
        "lock" => "lock",
        "sql" | "sqlite" | "db" => "database",
        "graphql" | "gql" => "graphql",
        "proto" => "proto",
        "nix" => "nix",
        // Xcode's own files; Material Icon Theme has no Xcode icon.
        "plist" | "pbxproj" | "xcconfig" | "xcscheme" | "entitlements" | "ini" | "cfg" | "conf"
        | "config" | "properties" => "settings",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        for (name, want) in [
            ("main.rs", "rust"),
            ("MAIN.RS", "rust"),
            ("go.mod", "go-mod"),
            ("Package.swift", "swift"),
            ("Cargo.toml", "rust"),
            ("index.d.ts", "typescript-def"),
            ("app.test.ts", "test-ts"),
            ("README.md", "readme"),
            ("Makefile", "makefile"),
            (".env", "tune"),
            (".env.local", "tune"),
            ("Dockerfile", "docker"),
            ("photo.JPG", "image"),
        ] {
            assert_eq!(kind(name), Some(want), "{name}");
            assert!(file_icon(name, true).is_some(), "{name}");
        }
        assert_eq!(kind("foo.unknown"), None);
        assert!(file_icon("foo.unknown", false).is_none());
    }

    #[test]
    fn folder_kinds() {
        for (name, want) in [
            ("src/", "folder-src"),
            ("Tests", "folder-test"),
            ("__tests__", "folder-test"),
            (".github", "folder-github"),
            ("crates", "folder-lib"),
            ("target/", "folder-target"),
        ] {
            assert_eq!(folder_kind(name), Some(want), "{name}");
            assert!(folder_icon(name, false).is_some(), "{name}");
        }
        assert_eq!(folder_kind("frobnicate/"), None);
    }

    #[test]
    fn light_variant() {
        let id = |dark| file_icon("a.toml", dark).unwrap().id();
        assert_ne!(id(true), id(false));
        assert_eq!(
            file_icon("a.rs", true).unwrap().id(),
            file_icon("a.rs", false).unwrap().id()
        );
    }

    #[test]
    fn svgs_parse_at_16px() {
        // GPUI rasterizes an SVG at twice its own size: 16px gives 32px, a 960 viewBox 1920px.
        for (n, svg) in SVGS {
            let tree = resvg::usvg::Tree::from_data(svg, &Default::default()).unwrap();
            assert_eq!(
                (tree.size().width(), tree.size().height()),
                (16., 16.),
                "{n}"
            );
        }
    }
}
