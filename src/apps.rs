//! Apps a workspace, repo or Xcode project can be opened in: every app LaunchServices
//! lists as an editor of folders, plus Xcode. Found once per launch, icons included.
use gpui_kit::{Image, ImageFormat};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Kind {
    /// Opens a folder in a shell; listed first.
    Terminal,
    /// Opens any folder: editors, terminals LaunchServices doesn't call shells.
    Folder,
    /// Opens a repo, not the workspace folder around them.
    Git,
    /// Opens an `.xcworkspace`, `.xcodeproj` or `Package.swift`.
    Xcode,
}

pub(crate) struct OpenApp {
    pub name: String,
    pub path: PathBuf,
    pub kind: Kind,
    pub icon: Option<Arc<Image>>,
}

const XCODE: &str = "com.apple.dt.Xcode";
const FINDER: &str = "com.apple.finder";
/// Git clients register for folders but only open repos.
const GIT: &[&str] = &[
    "com.DanPristupov.Fork",
    "com.fournova.Tower3",
    "com.sublimemerge",
    "com.github.GitHubClient",
    "com.torusknot.SourceTreeNotMac",
    "com.axosoft.gitkraken",
    "co.gitup.mac",
];
/// IDEs that open a folder as a project but register no folder type with LaunchServices.
const IDES: &[&str] = &[
    "com.google.android.studio",
    "com.jetbrains.intellij",
    "com.jetbrains.intellij.ce",
    "com.jetbrains.WebStorm",
    "com.jetbrains.pycharm",
    "com.jetbrains.pycharm.ce",
    "com.jetbrains.goland",
    "com.jetbrains.CLion",
    "com.jetbrains.rider",
    "com.jetbrains.rustrover",
    "com.jetbrains.PhpStorm",
    "com.jetbrains.rubymine",
    "com.jetbrains.fleet",
    "dev.zed.Zed",
    "com.todesktop.230313mzl4w4u92", // Cursor
    "com.sublimetext.4",
    "com.panic.Nova",
    "dev.warp.Warp-Stable",
];
/// Folder handlers that aren't a way to work on code.
const SKIP: &[&str] = &[
    "com.apple.Notes",
    "io.github.kaffediem.shark",
    "com.anthropic.claudefordesktop",
];

// ponytail: found once per launch; apps installed while Shark runs show after a restart.
pub(crate) fn installed() -> &'static [OpenApp] {
    static APPS: OnceLock<Vec<OpenApp>> = OnceLock::new();
    #[cfg(test)]
    return APPS.get_or_init(fake_apps);
    #[cfg(not(test))]
    APPS.get_or_init(discover)
}

impl OpenApp {
    pub(crate) fn open(&self, path: &Path) {
        #[cfg(test)]
        OPENED
            .lock()
            .unwrap()
            .push((self.name.clone(), path.to_owned()));
        #[cfg(not(test))]
        let _ = std::process::Command::new("open")
            .arg("-a")
            .arg(&self.path)
            .arg(path)
            .spawn();
    }

    /// What this app opens, as (label, path): `folder` (the workspace's, if any) and the
    /// repos for terminals and editors, the repos for git clients, their projects for Xcode.
    pub(crate) fn targets(
        &self,
        folder: Option<&(String, PathBuf)>,
        repos: &[(String, PathBuf)],
    ) -> Vec<(String, PathBuf)> {
        match self.kind {
            Kind::Terminal | Kind::Folder => folder.into_iter().chain(repos).cloned().collect(),
            Kind::Git => repos.to_vec(),
            Kind::Xcode => (repos.iter())
                .flat_map(|(name, repo)| {
                    xcode_projects(repo).into_iter().map(move |p| {
                        let rel = p.strip_prefix(repo).unwrap_or(&p).display();
                        (format!("{name}/{rel}"), p)
                    })
                })
                .collect(),
        }
    }
}

/// Folders a project search never enters: dependencies and build output.
const SKIP_DIRS: &[&str] = &[
    "Pods",
    "Carthage",
    "DerivedData",
    "build",
    "node_modules",
    "vendor",
    "target",
];

/// The Xcode projects in `repo` and two folders down, shallowest first. In each folder, its
/// `.xcworkspace`s and the `.xcodeproj`s no workspace there is named after (CocoaPods'
/// `App.xcworkspace` holds `App.xcodeproj`); a folder with neither offers its `Package.swift`,
/// unless the repo has a project.
pub(crate) fn xcode_projects(repo: &Path) -> Vec<PathBuf> {
    // ponytail: walks the disk on every menu open; cache per repo if a big repo makes menus lag.
    let mut found = vec![];
    let mut dirs = vec![repo.to_path_buf()];
    for _depth in 0..3 {
        let mut next = vec![];
        for dir in dirs {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            // The entry's own type: symlinks aren't followed, and there's no extra stat.
            let mut subdirs: Vec<PathBuf> = (rd.flatten())
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path())
                .collect();
            subdirs.sort();
            let ext = |p: &Path, x: &str| p.extension().is_some_and(|e| e == x);
            let stem = |p: &Path| p.file_stem().map(|s| s.to_owned());
            let ws: Vec<_> = subdirs.iter().filter(|p| ext(p, "xcworkspace")).collect();
            let mut here: Vec<PathBuf> = (subdirs.iter())
                .filter(|p| ext(p, "xcworkspace") || ext(p, "xcodeproj"))
                .filter(|p| !ext(p, "xcodeproj") || !ws.iter().any(|w| stem(w) == stem(p)))
                .cloned()
                .collect();
            let package = dir.join("Package.swift");
            if here.is_empty() && package.is_file() {
                here.push(package);
            }
            found.extend(here);
            next.extend(subdirs.into_iter().filter(|p| {
                let name = p.file_name().unwrap_or_default().to_string_lossy();
                let bundle = ["xcodeproj", "xcworkspace", "app", "framework", "bundle"]
                    .iter()
                    .any(|x| ext(p, x));
                !name.starts_with('.') && !bundle && !SKIP_DIRS.contains(&name.as_ref())
            }));
        }
        dirs = next;
    }
    // Local packages next to an app would make every open ask; the app is the one wanted.
    if found.iter().any(|p| !p.ends_with("Package.swift")) {
        found.retain(|p| !p.ends_with("Package.swift"));
    }
    found
}

#[cfg(test)]
pub(crate) static OPENED: std::sync::Mutex<Vec<(String, PathBuf)>> = std::sync::Mutex::new(vec![]);

/// Tests see a fixed set of apps, and nothing launches.
#[cfg(test)]
fn fake_apps() -> Vec<OpenApp> {
    [
        ("Terminal", Kind::Terminal),
        ("Code", Kind::Folder),
        ("Fork", Kind::Git),
        ("Xcode", Kind::Xcode),
    ]
    .into_iter()
    .map(|(name, kind)| OpenApp {
        name: name.into(),
        path: format!("/Applications/{name}.app").into(),
        kind,
        icon: None,
    })
    .collect()
}

fn discover() -> Vec<OpenApp> {
    let shells = handlers("public.unix-executable", LSRolesMask::Shell);
    let mut ids = handlers("public.folder", LSRolesMask::Editor);
    // Terminal, Ghostty and Fork register for `public.directory`, which `public.folder` misses.
    ids.extend(handlers("public.directory", LSRolesMask::Editor));
    // Finder too: cmd-o is the one way to open a workspace somewhere.
    ids.extend(
        IDES.iter()
            .chain([&XCODE, &FINDER])
            .map(|id| id.to_string()),
    );
    let mut apps: Vec<OpenApp> = vec![];
    for id in ids {
        if SKIP.contains(&id.as_str()) {
            continue;
        }
        let ns = NSWorkspace::sharedWorkspace();
        let Some(path) = ns
            .URLForApplicationWithBundleIdentifier(&NSString::from_str(&id))
            .and_then(|u| u.path())
            .map(|p| PathBuf::from(p.to_string()))
        else {
            continue;
        };
        if apps.iter().any(|a| a.path == path) {
            continue;
        }
        let kind = match id.as_str() {
            XCODE => Kind::Xcode,
            _ if GIT.contains(&id.as_str()) => Kind::Git,
            _ if shells.contains(&id) => Kind::Terminal,
            _ => Kind::Folder,
        };
        apps.push(OpenApp {
            // The bundle's file name: VS Code's CFBundleName is just "Code".
            name: path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            icon: icon_png(&path).map(|png| Arc::new(Image::from_bytes(ImageFormat::Png, png))),
            path,
            kind,
        });
    }
    apps.sort_by_key(|a| (a.kind != Kind::Terminal, a.name.to_lowercase()));
    apps
}

use objc2_app_kit::NSWorkspace;
use objc2_core_services::LSRolesMask;
use objc2_foundation::NSString;

/// Bundle ids of the apps with `role` for files of type `uti`.
#[allow(deprecated)] // the role-filtered lookup has no non-deprecated replacement
fn handlers(uti: &str, role: LSRolesMask) -> Vec<String> {
    use objc2_core_foundation::{CFArray, CFString};
    let uti = CFString::from_str(uti);
    let Some(ids) =
        (unsafe { objc2_core_services::LSCopyAllRoleHandlersForContentType(&uti, role) })
    else {
        return vec![];
    };
    // SAFETY: LaunchServices returns an array of bundle-id strings.
    let ids: &CFArray<CFString> = unsafe { ids.cast_unchecked() };
    ids.iter().map(|s| s.to_string()).collect()
}

/// The app's icon at switcher size, as a PNG; AppKit reads both `.icns` and asset catalogs.
pub(crate) fn icon_png(app: &Path) -> Option<Vec<u8>> {
    use objc2::AllocAnyThread as _;
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep};
    use objc2_foundation::{NSDictionary, NSPoint, NSRect, NSSize};
    let img =
        NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(&app.to_string_lossy()));
    let mut rect = NSRect::new(NSPoint::new(0., 0.), NSSize::new(128., 128.));
    let cg = unsafe { img.CGImageForProposedRect_context_hints(&mut rect, None, None) }?;
    let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &cg);
    let png = unsafe {
        rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }?;
    Some(png.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("shark-apps-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn xcode_projects_prefer_workspaces_and_skip_bundles_and_deps() {
        let r = tmp("xc");
        for d in [
            "App.xcodeproj/project.xcworkspace", // inside a project: never a match
            "App.xcworkspace",
            "Tool.xcodeproj", // not App.xcworkspace's: still offered
            "ios/Sub.xcodeproj",
            "ios/B.xcodeproj",
            "pkgs/Kit",
            "MyApp.iOS/M.xcodeproj", // a dot in a plain folder's name
            "Pods/Pods.xcodeproj",
            "node_modules/x/X.xcodeproj",
            ".hidden/H.xcodeproj",
            "Shark.app/Contents/In.xcodeproj",
            "a/b/c/TooDeep.xcodeproj",
            "elsewhere/Linked.xcodeproj",
        ] {
            fs::create_dir_all(r.join(d)).unwrap();
        }
        fs::write(r.join("pkgs/Kit/Package.swift"), "").unwrap(); // dropped: there's an app
        fs::write(r.join("ios/NotADir.xcodeproj"), "").unwrap();
        // A symlinked folder isn't followed: no project found twice, no loops.
        std::os::unix::fs::symlink(&r, r.join("ios/loop")).unwrap();
        std::os::unix::fs::symlink(r.join("elsewhere"), r.join("Linked")).unwrap();
        assert_eq!(
            xcode_projects(&r),
            [
                r.join("App.xcworkspace"),
                r.join("Tool.xcodeproj"),
                r.join("MyApp.iOS/M.xcodeproj"),
                r.join("elsewhere/Linked.xcodeproj"),
                r.join("ios/B.xcodeproj"),
                r.join("ios/Sub.xcodeproj"),
            ]
        );
        let pkg = tmp("pkg");
        fs::create_dir_all(pkg.join("Sources")).unwrap();
        fs::create_dir_all(pkg.join("Example")).unwrap();
        fs::write(pkg.join("Package.swift"), "").unwrap();
        fs::write(pkg.join("Example/Package.swift"), "").unwrap();
        assert_eq!(
            xcode_projects(&pkg),
            [pkg.join("Package.swift"), pkg.join("Example/Package.swift")],
            "a package repo offers its packages"
        );
        let plain = tmp("plain");
        fs::create_dir_all(plain.join("src")).unwrap();
        assert!(xcode_projects(&plain).is_empty());
        assert!(xcode_projects(&tmp("missing")).is_empty());
    }

    /// The real lookup on this Mac: Terminal and Finder ship with macOS.
    #[test]
    fn discover_finds_terminal_with_its_icon() {
        let apps = discover();
        let t = apps
            .iter()
            .find(|a| a.name == "Terminal")
            .expect("Terminal found");
        assert_eq!(t.kind, Kind::Terminal);
        assert!(
            t.icon
                .as_ref()
                .is_some_and(|i| i.bytes.starts_with(b"\x89PNG"))
        );
        assert!((apps.iter()).all(|a| !["Shark", "Claude"].contains(&a.name.as_str())));
        let finder = apps
            .iter()
            .find(|a| a.name == "Finder")
            .expect("Finder listed");
        assert_eq!(
            finder.kind,
            Kind::Folder,
            "opens the workspace folder and repos"
        );
        assert_eq!(apps[0].kind, Kind::Terminal, "terminals first");
    }

    #[test]
    fn targets_by_kind() {
        let (ios, web) = (tmp("t-ios"), tmp("t-web"));
        fs::create_dir_all(ios.join("App.xcodeproj")).unwrap();
        fs::create_dir_all(&web).unwrap();
        let folder = ("Workspace folder".to_string(), PathBuf::from("/ws"));
        let repos = [("ios".to_string(), ios.clone()), ("web".to_string(), web)];
        let app = |kind| OpenApp {
            name: "x".into(),
            path: "/x.app".into(),
            kind,
            icon: None,
        };
        let labels = |kind| -> Vec<String> {
            (app(kind).targets(Some(&folder), &repos).into_iter())
                .map(|t| t.0)
                .collect()
        };
        assert_eq!(labels(Kind::Terminal), ["Workspace folder", "ios", "web"]);
        assert_eq!(labels(Kind::Folder), ["Workspace folder", "ios", "web"]);
        assert_eq!(labels(Kind::Git), ["ios", "web"]);
        assert_eq!(labels(Kind::Xcode), ["ios/App.xcodeproj"]);
        assert_eq!(
            app(Kind::Xcode).targets(None, &repos)[0].1,
            ios.join("App.xcodeproj")
        );
    }
}
