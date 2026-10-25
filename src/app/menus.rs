//! Right-click menus and the path helpers they share.
use super::*;

/// `/Users/me/x` → `~/x`: paths in menus are read, so keep them short.
pub(super) fn tilde(path: &Path) -> String {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    match path.strip_prefix(&home) {
        Ok(rest) if !home.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => path.display().to_string(),
    }
}

// ponytail: always Terminal.app; add a setting if people want iTerm/Ghostty/Warp.
pub(super) fn open_terminal(path: &Path) {
    let _ = std::process::Command::new("open")
        .args(["-a", "Terminal"])
        .arg(path)
        .spawn();
}

pub(super) fn copy(text: String, window: &mut Window, cx: &mut App) {
    window.push_notification(format!("Copied {text}"), cx);
    cx.write_to_clipboard(ClipboardItem::new_string(text));
}

/// "Workspace folder", its path muted on the right, plus one row per repo.
pub(super) fn path_submenu(
    icon: IconName,
    label: &'static str,
    paths: &[(String, PathBuf)],
    f: fn(&Path, &mut Window, &mut App),
) -> Entry {
    let mut entries = vec![];
    for (ix, (name, path)) in paths.iter().enumerate() {
        if ix == 1 {
            entries.push(Entry::Sep);
        }
        let p = path.clone();
        let icon = if ix == 0 {
            IconName::Folder
        } else {
            IconName::FolderGit2
        };
        // The repo rows' labels already say which; only the folder row needs its path.
        let item = menu::item(icon, name.clone(), move |w, cx| f(&p, w, cx));
        entries.push(if ix == 0 { item.hint(tilde(path)) } else { item });
    }
    menu::sub(icon, label, entries)
}

/// Right-click menu of a sidebar workspace. Shows where everything lives on disk,
/// and keeps the destructive items last. Shortcuts live in the palette, not here (Apple HIG).
pub(super) fn ws_menu(this: &Shark, ws: &Workspace, cx: &Context<Shark>) -> Vec<Entry> {
    let paths: Vec<(String, PathBuf)> = std::iter::once(("Workspace folder".into(), ws.dir()))
        .chain(
            ws.repos
                .iter()
                .map(|r| (r.name.clone(), ws.dir().join(&r.name))),
        )
        .collect();
    let pr_urls = this
        .syncs
        .get(&ws.name)
        .map(|s| s.read(cx).pr_urls())
        .unwrap_or_default();
    let branch = ws.branch.clone();
    let (n1, n2, n3) = (ws.name.clone(), ws.name.clone(), ws.name.clone());
    let mut entries = vec![];
    if this.active.as_deref() != Some(ws.name.as_str()) {
        // By name: the list can change under an open menu (a background auto-archive).
        entries.push(menu::item(
            IconName::MessageSquare,
            "Open workspace",
            on_shark(cx, move |s, w, cx| {
                if let Some(ix) = s.state.workspaces.iter().position(|w| w.name == n1) {
                    s.open_workspace(ix, w, cx)
                }
            }),
        ));
    }
    entries.extend([
        move_submenu(this, &ws.name, cx),
        Entry::Sep,
        path_submenu(
            IconName::FolderOpen,
            "Reveal in Finder",
            &paths,
            |p, _, cx| cx.reveal_path(p),
        ),
        path_submenu(
            IconName::SquareTerminal,
            "Open in Terminal",
            &paths,
            |p, _, _| open_terminal(p),
        ),
        path_submenu(IconName::Copy, "Copy path", &paths, |p, w, cx| {
            copy(p.display().to_string(), w, cx)
        }),
        menu::item(IconName::GitBranch, "Copy branch name", move |w, cx| {
            copy(branch.clone(), w, cx)
        })
        // The branch is usually the workspace's name; then the hint says nothing new.
        .hint(if ws.branch == ws.name { "" } else { ws.branch.as_str() }.to_string()),
    ]);
    if !pr_urls.is_empty() {
        let label = match pr_urls.len() {
            1 => "Open pull request".to_string(),
            n => format!("Open {n} pull requests"),
        };
        entries.push(Entry::Sep);
        entries.push(menu::item(IconName::ExternalLink, label, move |_, cx| {
            pr_urls.iter().for_each(|u| cx.open_url(u))
        }));
    }
    entries.extend([
        Entry::Sep,
        menu::item(
            IconName::Archive,
            "Archive",
            on_shark(cx, move |s, w, cx| s.archive_ws(n2.clone(), false, w, cx)),
        ),
        menu::item(
            IconName::ArchiveX,
            "Force archive…",
            on_shark(cx, move |s, w, cx| s.archive_ws(n3.clone(), true, w, cx)),
        ),
    ]);
    entries
}

/// A menu callback that runs `f` on Shark.
pub(super) fn on_shark(
    cx: &Context<Shark>,
    f: impl Fn(&mut Shark, &mut Window, &mut Context<Shark>) + 'static,
) -> impl Fn(&mut Window, &mut App) + 'static {
    let shark = cx.entity().downgrade();
    move |window, cx| {
        shark.update(cx, |s, cx| f(s, window, cx)).ok();
    }
}
