//! Right-click menus and the path helpers they share.
use super::*;
use crate::apps::{self, Kind, OpenApp};

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

/// "Open in <app>" for each installed app with something to open in `folder` (the
/// workspace's, if any) and `repos`: one target opens directly, several make a submenu.
pub(super) fn open_in_entries(
    folder: Option<&(String, PathBuf)>,
    repos: &[(String, PathBuf)],
) -> Vec<Entry> {
    let mut entries = vec![];
    for app in apps::installed() {
        let mut targets = app.targets(folder, repos);
        if targets.is_empty() {
            continue;
        }
        let label = format!("Open in {}", app.name);
        let image = app.icon.clone();
        if targets.len() == 1 {
            let (hint, path) = targets.remove(0);
            let item = menu::item(app_icon(app), label, move |_, _| app.open(&path));
            entries.push(item.hint(hint).image(image));
            continue;
        }
        let rows = targets.into_iter().map(|(name, path)| {
            let is_folder = folder.is_some_and(|f| f.1 == path);
            let icon = match app.kind {
                _ if is_folder => IconName::Folder,
                Kind::Xcode => IconName::FileCode,
                _ => IconName::FolderGit2,
            };
            let hint = if is_folder {
                tilde(&path)
            } else {
                String::new()
            };
            menu::item(icon, name, move |_, _| app.open(&path)).hint(hint)
        });
        entries.push(menu::sub(app_icon(app), label, rows.collect()).image(image));
    }
    entries
}

/// Shown when AppKit has no icon for the app.
pub(super) fn app_icon(app: &OpenApp) -> IconName {
    match app.kind {
        Kind::Terminal => IconName::SquareTerminal,
        Kind::Folder => IconName::AppWindow,
        Kind::Git => IconName::GitBranch,
        Kind::Xcode => IconName::FileCode,
    }
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
        entries.push(if ix == 0 {
            item.hint(tilde(path))
        } else {
            item
        });
    }
    menu::sub(icon, label, entries)
}

/// Right-click menu of a sidebar workspace. Shows where everything lives on disk,
/// and keeps the destructive items last. Shortcuts live in the palette, not here (Apple HIG).
pub(super) fn ws_menu(this: &Shark, ws: &Workspace, cx: &Context<Shark>) -> Vec<Entry> {
    let paths: Vec<(String, PathBuf)> = std::iter::once(("Workspace folder".into(), ws.dir()))
        .chain(super::palette::repo_paths(ws))
        .collect();
    let (n1, n2, n3) = (ws.name.clone(), ws.name.clone(), ws.name.clone());
    let mut entries = vec![];
    if this.home_shown() || this.active.as_deref() != Some(ws.name.as_str()) {
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
    entries.push(move_submenu(this, &ws.name, cx));
    let missing: Vec<Entry> = (this.state.repos.iter())
        .filter(|r| !ws.repos.iter().any(|w| w.name == r.name))
        .map(|r| {
            let (name, repo) = (ws.name.clone(), r.clone());
            menu::item(
                IconName::FolderGit2,
                r.name.clone(),
                on_shark(cx, move |s, w, cx| {
                    s.add_repo_to_workspace(name.clone(), repo.clone(), w, cx)
                }),
            )
        })
        .collect();
    if !missing.is_empty() {
        entries.push(menu::sub(IconName::FolderPlus, "Add repo", missing));
    }
    entries.extend([
        Entry::Sep,
        path_submenu(
            IconName::FolderOpen,
            "Reveal in Finder",
            &paths,
            |p, _, cx| cx.reveal_path(p),
        ),
    ]);
    entries.extend(open_in_entries(Some(&paths[0]), &paths[1..]));
    entries.extend([path_submenu(
        IconName::Copy,
        "Copy path",
        &paths,
        |p, w, cx| copy(p.display().to_string(), w, cx),
    )]);
    entries.extend(branch_entries(this, ws, cx));
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

/// One repo's PR stack around the workspace's branch, trunk side first.
pub(super) struct RepoStack {
    pub repo: String,
    pub nodes: Vec<crate::stack::Node>,
    /// The workspace's own branch.
    pub me: usize,
}

/// Each repo of `ws` that is part of a stack (a chain of two or more branches), from the
/// local workspaces and the open PRs the git button keeps. Empty until the repo was probed.
pub(super) fn stacks(this: &Shark, ws: &Workspace, cx: &App) -> Vec<RepoStack> {
    let Some(sync) = this.syncs.get(&ws.name).map(|s| s.read(cx)) else {
        return vec![];
    };
    let probed = sync.repos().unwrap_or_default();
    (ws.repos.iter())
        .filter_map(|r| {
            let trunk = &probed
                .iter()
                .find(|p| p.name == r.name)?
                .local
                .as_ref()
                .ok()?
                .trunk;
            if trunk.is_empty() {
                return None;
            }
            let local: Vec<(String, String, String)> = (this.state.workspaces.iter())
                .filter_map(|w| {
                    let wr = w.repos.iter().find(|x| x.src == r.src)?;
                    Some((w.name.clone(), w.branch_of(wr).to_string(), wr.base.clone()))
                })
                .collect();
            let prs = sync.stack_prs.get(&r.name).map_or(&[][..], Vec::as_slice);
            let (nodes, me) = crate::stack::chain(ws.branch_of(r), trunk, &local, prs);
            (nodes.len() > 1).then(|| RepoStack {
                repo: r.name.clone(),
                nodes,
                me,
            })
        })
        .collect()
}

/// The header's `base ← branch`: the stacked repos' parent when they share one (a backend repo
/// on `main` next to a stacked iOS repo still reads `logic ← ui`), else the base every repo
/// shares, else the branch alone.
pub(super) fn branch_label(this: &Shark, ws: &Workspace, cx: &App) -> String {
    let probed = (this.syncs.get(&ws.name))
        .and_then(|s| s.read(cx).repos().map(<[_]>::to_vec))
        .unwrap_or_default();
    let parents: Vec<&str> = probed.iter().filter_map(|r| r.stacked()).collect();
    let bases: Vec<&str> = match parents.is_empty() {
        false => parents,
        true => (ws.repos.iter())
            .filter(|r| r.base.starts_with("origin/"))
            .map(|r| crate::sync::short(&r.base))
            .collect(),
    };
    crate::stack::title(&ws.branch, &bases)
}

/// A stack's rows, newest on top, the workspace's own checked. A row opens its workspace,
/// else its PR; a merged parent (gone from the open PRs) shows the PR the git button found.
fn stack_rows(
    this: &Shark,
    ws: &Workspace,
    s: &RepoStack,
    prefix: bool,
    cx: &Context<Shark>,
) -> Vec<Entry> {
    let sync = this.syncs.get(&ws.name).map(|g| g.read(cx));
    let parent = (sync.and_then(|g| g.repos()))
        .and_then(|rs| rs.iter().find(|r| r.name == s.repo))
        .and_then(|r| Some((r.stacked()?.to_string(), r.parent.clone()?)));
    (s.nodes.iter().enumerate().rev())
        .map(|(i, n)| {
            let merged = parent
                .as_ref()
                .filter(|(b, _)| *b == n.branch && n.pr.is_none());
            let (icon, status, url) = match (&n.pr, merged) {
                (Some(p), _) => {
                    let icon = match p.status() {
                        complete::PrStatus::Draft => IconName::GitPullRequestDraft,
                        _ => IconName::GitPullRequest,
                    };
                    (
                        icon,
                        format!("#{} · {}", p.number, p.status().label()),
                        Some(p.url.clone()),
                    )
                }
                (None, Some((_, p))) => {
                    let label = format!("#{} · {:?}", p.number, p.state).to_lowercase();
                    (IconName::GitMerge, label, Some(p.url.clone()))
                }
                (None, None) if n.ws.is_some() => (IconName::GitBranch, "workspace".into(), None),
                (None, None) => (IconName::GitBranch, String::new(), None),
            };
            let hint = match prefix {
                true => [s.repo.as_str(), &status]
                    .iter()
                    .filter(|x| !x.is_empty())
                    .copied()
                    .collect::<Vec<_>>()
                    .join(" · "),
                false => status,
            };
            let target = n.ws.clone().filter(|w| *w != ws.name);
            menu::item(
                icon,
                n.branch.clone(),
                on_shark(cx, move |s, w, cx| match (&target, &url) {
                    (Some(name), _) => {
                        if let Some(ix) = s.state.workspaces.iter().position(|x| &x.name == name) {
                            s.open_workspace(ix, w, cx)
                        }
                    }
                    (None, Some(u)) => cx.open_url(u),
                    _ => {}
                }),
            )
            .hint(hint)
            .checked(i == s.me)
        })
        .collect()
}

/// The stack rows (one repo's inline, several as a submenu each), then restack, new on top,
/// insert below, copy and change base, then what's on GitHub. Shared by the sidebar's and the
/// header's menus.
pub(super) fn branch_entries(this: &Shark, ws: &Workspace, cx: &Context<Shark>) -> Vec<Entry> {
    let sync = this.syncs.get(&ws.name).map(|s| s.read(cx));
    let branches = sync.map(|s| s.branches()).unwrap_or_default();
    // Not opened yet, so not probed: the branches the workspace put them on.
    let saved = || {
        ws.repos
            .iter()
            .map(|r| (r.name.clone(), ws.branch_of(r).to_string()))
    };
    let pairs: Vec<(String, String)> = match branches.is_empty() {
        true => saved().collect(),
        false => branches
            .iter()
            .map(|b| (b.repo.clone(), b.name.clone()))
            .collect(),
    };
    let mut names: Vec<String> = pairs.iter().map(|(_, b)| b.clone()).collect();
    names.sort();
    names.dedup();
    // One name when they share a branch, else one per repo.
    let copies = match names.len() {
        0 | 1 => {
            let b = names.pop().unwrap_or_else(|| ws.branch.clone());
            // The branch is usually the workspace's name; then the hint says nothing new.
            let hint = if b == ws.name {
                String::new()
            } else {
                b.clone()
            };
            vec![(hint, String::new(), b)]
        }
        _ => (pairs.into_iter())
            .map(|(repo, b)| (repo, b.clone(), b))
            .collect(),
    };
    let links = (branches.into_iter())
        .filter_map(|b| Some((b.repo, String::new(), b.url?)))
        .collect();
    let name = ws.name.clone();
    let base = menu::item(
        IconName::Target,
        "Change base…",
        on_shark(cx, move |s, w, cx| s.change_base(name.clone(), w, cx)),
    )
    .hint(base_hint(ws));
    let stacks = stacks(this, ws, cx);
    let mut entries: Vec<Entry> = match &stacks[..] {
        [one] => stack_rows(this, ws, one, ws.repos.len() > 1, cx),
        many => (many.iter())
            .map(|s| {
                let rows = stack_rows(this, ws, s, false, cx);
                let hint = format!("{} of {}", s.me + 1, s.nodes.len());
                menu::sub(IconName::Layers, format!("Stack · {}", s.repo), rows).hint(hint)
            })
            .collect(),
    };
    if !entries.is_empty() {
        entries.push(Entry::Sep);
    }
    entries.extend(stack_actions(ws, sync, cx));
    entries.extend(
        [
            each_or_all(
                IconName::GitBranch,
                "Copy branch name",
                copies,
                |bs, w, cx| {
                    let mut bs = bs.to_vec();
                    bs.sort();
                    bs.dedup();
                    copy(bs.join("\n"), w, cx)
                },
            ),
            Some(base),
        ]
        .into_iter()
        .flatten(),
    );
    let github: Vec<Entry> = [
        each_or_all(IconName::Github, "Open branch on GitHub", links, open_urls),
        sync.and_then(pr_entry),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !github.is_empty() {
        entries.push(Entry::Sep);
        entries.extend(github);
    }
    entries
}

/// Restack onto the parent (the merged parent's base once it merged), new workspace on top,
/// insert one below. Review checkouts get none of them.
fn stack_actions(
    ws: &Workspace,
    sync: Option<&crate::sync::GitSync>,
    cx: &Context<Shark>,
) -> Vec<Entry> {
    let repos = sync.and_then(|g| g.repos()).unwrap_or_default();
    if repos
        .iter()
        .any(|r| r.local.as_ref().is_ok_and(|l| l.review))
    {
        return vec![];
    }
    let name = ws.name.clone();
    let merged: Vec<String> = (repos.iter().filter(|r| r.parent_merged()))
        .map(|r| r.name.clone())
        .collect();
    let stacked: Vec<&crate::sync::Repo> = repos.iter().filter(|r| r.stacked().is_some()).collect();
    let mut out = vec![];
    if let Some(onto) = (repos.iter().find(|r| r.parent_merged())).and_then(|r| r.parent.as_ref()) {
        let (label, n) = (
            format!("Parent merged · rebase onto {}", onto.base),
            name.clone(),
        );
        out.push(
            menu::item(
                IconName::Layers,
                label.clone(),
                on_shark(cx, move |s, w, cx| {
                    if let Some(g) = s.syncs.get(&n).cloned() {
                        let (label, rs) = (label.clone(), merged.clone());
                        g.update(cx, |g, cx| {
                            g.run_kind_in(crate::sync::Kind::Restack, &label, rs, w, cx)
                        });
                    }
                }),
            )
            .hint("Agent"),
        );
    } else if let Some(r) = stacked.first() {
        let p = r.stacked().unwrap_or_default();
        let label = match stacked.len() {
            1 => format!("Rebase onto {p}"),
            _ => "Rebase onto parent".to_string(),
        };
        let behind = r.local.as_ref().map_or(0, |l| l.base.1);
        let n = name.clone();
        out.push(
            menu::item(
                IconName::GitCompareArrows,
                label,
                on_shark(cx, move |s, w, cx| {
                    if let Some(g) = s.syncs.get(&n).cloned() {
                        g.update(cx, |g, cx| g.restack_onto_parent(w, cx));
                    }
                }),
            )
            .hint(match behind {
                0 => "Agent".to_string(),
                n => format!("{n} behind"),
            }),
        );
    }
    let (n1, n2) = (name.clone(), name.clone());
    out.push(
        menu::item(
            IconName::Layers,
            "New workspace on top…",
            on_shark(cx, move |s, w, cx| s.stack_from(n1.clone(), false, w, cx)),
        )
        .hint(ws.branch.clone()),
    );
    let below = match stacked.first().and_then(|r| r.stacked()) {
        Some(p) => format!("between {p} and {}", ws.branch),
        None => format!("under {}", ws.branch),
    };
    out.push(
        menu::item(
            IconName::ListPlus,
            "Insert workspace below…",
            on_shark(cx, move |s, w, cx| s.stack_from(n2.clone(), true, w, cx)),
        )
        .hint(below),
    );
    out
}

/// "Open pull request" for each repo with one, or `None` before any PR.
pub(crate) fn pr_entry(sync: &crate::sync::GitSync) -> Option<Entry> {
    let prs = (sync.pr_urls().into_iter())
        .map(|(repo, url)| (repo, String::new(), url))
        .collect();
    each_or_all(
        IconName::GitPullRequest,
        "Open pull request",
        prs,
        open_urls,
    )
}

fn open_urls(urls: &[String], _: &mut Window, cx: &mut App) {
    urls.iter().for_each(|u| cx.open_url(u))
}

/// The base every repo targets, e.g. `origin/main`, or "per repo" when they differ.
pub(super) fn base_hint(ws: &Workspace) -> String {
    let mut bases: Vec<&str> = ws.repos.iter().map(|r| r.base.as_str()).collect();
    bases.sort();
    bases.dedup();
    match bases[..] {
        [b] => b.to_string(),
        [] => String::new(),
        _ => "per repo".into(),
    }
}

/// `label` for `rows` of `(label, hint, value)`, usually one per repo: a single row is one
/// item, hinted with its label; several make a submenu ending in "All". `f` gets the values.
pub(crate) fn each_or_all(
    icon: IconName,
    label: &'static str,
    mut rows: Vec<(String, String, String)>,
    f: impl Fn(&[String], &mut Window, &mut App) + 'static,
) -> Option<Entry> {
    let f = Rc::new(f);
    if rows.len() < 2 {
        let (hint, _, v) = rows.pop()?;
        let item = menu::item(icon, label, move |w, cx| f(std::slice::from_ref(&v), w, cx));
        return Some(item.hint(hint));
    }
    let all: Vec<String> = rows.iter().map(|r| r.2.clone()).collect();
    let mut entries: Vec<Entry> = (rows.into_iter())
        .map(|(name, hint, v)| {
            let f = f.clone();
            let run = move |w: &mut Window, cx: &mut App| f(std::slice::from_ref(&v), w, cx);
            menu::item(IconName::FolderGit2, name, run).hint(hint)
        })
        .collect();
    entries.extend([
        Entry::Sep,
        menu::item(None, "All", move |w, cx| f(&all, w, cx)),
    ]);
    Some(menu::sub(icon, label, entries))
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

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin

    #[test]
    fn each_or_all_collapses_one_row() {
        let row = |n: &str| (n.to_string(), String::new(), n.to_string());
        let f: fn(&[String], &mut Window, &mut App) = |_, _, _| {};
        assert!(each_or_all(IconName::Github, "Open", vec![], f).is_none());
        match each_or_all(IconName::Github, "Open", vec![row("api")], f) {
            Some(Entry::Item { label, hint, .. }) => {
                assert_eq!((&*label, hint.as_deref()), ("Open", Some("api")))
            }
            _ => panic!("one row is one item"),
        }
        match each_or_all(IconName::Github, "Open", vec![row("api"), row("web")], f) {
            Some(Entry::Sub { entries, .. }) => {
                let labels: Vec<_> = (entries.iter())
                    .map(|e| match e {
                        Entry::Item { label, .. } => label.to_string(),
                        _ => "-".into(),
                    })
                    .collect();
                assert_eq!(labels, ["api", "web", "-", "All"]);
            }
            _ => panic!("several rows are a submenu"),
        }
    }
}
