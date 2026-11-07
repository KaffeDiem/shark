//! Right-click menus and the path helpers they share.
use super::*;
use crate::apps::{self, Kind};
use crate::menu::each_or_all;
use crate::sync::PrState;

/// `/Users/me/x` → `~/x`: paths in menus are read, so keep them short.
pub(super) fn tilde(path: &Path) -> String {
    let home = crate::state::home();
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
    cx: &Context<Shark>,
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
            let item = menu::item(
                app_icon(app),
                label,
                on_shark(cx, move |s, _, _| s.launch_app(app, &path)),
            );
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
            menu::item(
                icon,
                name,
                on_shark(cx, move |s, _, _| s.launch_app(app, &path)),
            )
            .hint(hint)
        });
        entries.push(menu::sub(app_icon(app), label, rows.collect()).image(image));
    }
    entries
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
    let paths: Vec<(String, PathBuf)> = std::iter::once(("Agent folder".into(), ws.dir()))
        .chain(ws.repo_paths())
        .collect();
    let mut entries = vec![];
    if this.home_shown() || this.active.as_deref() != Some(ws.name.as_str()) {
        // By name: the list can change under an open menu (a background auto-archive).
        entries.push(menu::item(
            IconName::MessageSquare,
            "Open agent",
            on_ws(cx, &ws.name, |s, n, w, cx| {
                s.open_workspace_named(&n, w, cx)
            }),
        ));
    }
    entries.push(menu::item(
        IconName::Pencil,
        "Rename agent…",
        on_ws(cx, &ws.name, |s, n, w, cx| s.rename_agent(n, w, cx)),
    ));
    entries.push(move_submenu(this, &ws.name, cx));
    let missing: Vec<Entry> = (this.state.repos.iter())
        .filter(|r| !ws.repos.iter().any(|w| w.name == r.name))
        .map(|r| {
            let (name, repo) = (ws.name.clone(), r.clone());
            menu::item(
                IconName::FolderGit2,
                r.name.clone(),
                on_shark(cx, move |s, w, cx| {
                    s.attach_form(name.clone(), vec![repo.name.clone()], w, cx)
                }),
            )
        })
        .collect();
    // With no repos, the branch rows below offer it.
    if !missing.is_empty() && !ws.repos.is_empty() {
        entries.push(menu::sub(
            IconName::FolderPlus,
            "Attach repository",
            missing,
        ));
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
    entries.extend(open_in_entries(Some(&paths[0]), &paths[1..], cx));
    entries.extend([path_submenu(
        IconName::Copy,
        "Copy path",
        &paths,
        |p, w, cx| copy(p.display().to_string(), w, cx),
    )]);
    entries.extend(branch_entries(this, ws, None, cx));
    entries.extend([
        Entry::Sep,
        menu::item(
            IconName::Archive,
            "Delete agent…",
            on_ws(cx, &ws.name, |s, n, w, cx| s.archive_ws(n, false, w, cx)),
        ),
        menu::item(
            IconName::ArchiveX,
            "Force delete agent…",
            on_ws(cx, &ws.name, |s, n, w, cx| s.archive_ws(n, true, w, cx)),
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
                        crate::prs::PrStatus::Draft => IconName::GitPullRequestDraft,
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
                (None, None) if n.ws.is_some() => (IconName::GitBranch, "agent".into(), None),
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
                    (Some(name), _) => s.open_workspace_named(name, w, cx),
                    (None, Some(u)) => cx.open_url(u),
                    _ => {}
                }),
            )
            .hint(hint)
            .checked(i == s.me)
        })
        .collect()
}

/// The checked-out branch, with saved state before a successful probe; None means detached.
pub(super) fn repo_branch(
    this: &Shark,
    ws: &Workspace,
    repo: &crate::state::WsRepo,
    cx: &App,
) -> Option<String> {
    let local = this
        .syncs
        .get(&ws.name)
        .and_then(|s| s.read(cx).repos())
        .and_then(|rs| rs.iter().find(|r| r.name == repo.name))
        .and_then(|r| r.local.as_ref().ok());
    match local {
        Some(local) => local.branch.clone(),
        None => Some(ws.branch_of(repo).to_string()),
    }
}

/// Branch actions for one attached repository, or the whole workspace in its sidebar menu.
pub(super) fn branch_entries(
    this: &Shark,
    ws: &Workspace,
    repo: Option<&str>,
    cx: &Context<Shark>,
) -> Vec<Entry> {
    if repo.is_some_and(|name| !ws.repos.iter().any(|r| r.name == name)) {
        return vec![];
    }
    if ws.repos.is_empty() {
        return vec![menu::item(
            IconName::FolderPlus,
            "Attach repository…",
            on_ws(cx, &ws.name, |s, n, w, cx| s.attach_repository(n, w, cx)),
        )];
    }
    let sync = this.syncs.get(&ws.name).map(|s| s.read(cx));
    let pairs: Vec<(String, String)> = ws
        .repos
        .iter()
        .filter(|r| repo.is_none_or(|name| name == r.name))
        .filter_map(|r| Some((r.name.clone(), repo_branch(this, ws, r, cx)?)))
        .collect();
    let mut names: Vec<String> = pairs.iter().map(|(_, b)| b.clone()).collect();
    names.sort();
    names.dedup();
    // One name when they share a branch, else one per repo. Detached HEAD has no branch to copy.
    let copies = match names.len() {
        0 => vec![],
        1 => {
            let b = names.pop().unwrap();
            // A repository's menu is where its chip's branch is read, so it always names it.
            let hint = if b == ws.name && repo.is_none() {
                String::new()
            } else {
                b.clone()
            };
            vec![(hint, String::new(), b)]
        }
        _ => pairs
            .into_iter()
            .map(|(repo, b)| (repo, b.clone(), b))
            .collect(),
    };
    let mut links: Vec<(String, String, String)> = sync
        .and_then(|s| s.repos())
        .unwrap_or_default()
        .iter()
        .filter(|r| repo.is_none_or(|name| name == r.name))
        .filter_map(|r| Some((r.name.clone(), String::new(), r.branch_url(&r.branch)?)))
        .collect();
    let drifted = repo
        .and_then(|name| ws.repos.iter().find(|r| r.name == name))
        .filter(|r| repo_branch(this, ws, r, cx).as_deref() != Some(ws.branch_of(r)))
        .map(|r| ws.branch_of(r));
    let target = repo.map(str::to_string);
    let base = menu::item(
        IconName::Target,
        "Change base…",
        on_ws(cx, &ws.name, move |s, n, w, cx| {
            s.change_base(n, target.clone(), w, cx)
        }),
    )
    .hint(match repo {
        Some(name) => {
            let base = &ws.repos.iter().find(|r| r.name == name).unwrap().base;
            match drifted {
                Some(branch) => format!("{base} ← {branch}"),
                None => base.clone(),
            }
        }
        None => base_hint(ws),
    });
    // What a repository's chip card says, each on a row that acts on it.
    let worktree = repo.map(|name| {
        let path = ws.dir().join(name);
        let hint = tilde(&path);
        menu::item(IconName::Copy, "Copy path", move |w, cx| {
            copy(path.display().to_string(), w, cx)
        })
        .hint(hint)
    });
    let scoped = repo.and_then(|name| sync?.repos()?.iter().find(|r| r.name == name));
    let git_error = scoped.and_then(|r| r.local.as_ref().err()).map(|error| {
        let error = error.to_string();
        let hint = error.clone();
        menu::item(IconName::TriangleAlert, "Copy Git error", move |w, cx| {
            copy(error.clone(), w, cx)
        })
        .hint(hint)
    });
    let stacks: Vec<_> = stacks(this, ws, cx)
        .into_iter()
        .filter(|s| repo.is_none_or(|name| name == s.repo))
        .collect();
    let mut entries: Vec<Entry> = match &stacks[..] {
        [one] => stack_rows(this, ws, one, repo.is_none() && ws.repos.len() > 1, cx),
        many => many
            .iter()
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
    entries.extend(stack_actions(ws, sync, repo, drifted, cx));
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
            git_error,
        ]
        .into_iter()
        .flatten(),
    );
    let branch_link = match repo.and_then(|name| ws.repos.iter().find(|r| r.name == name)) {
        Some(r) if repo_branch(this, ws, r, cx).as_deref() != Some(ws.branch_of(r)) => {
            links.pop().map(|(_, _, url)| {
                menu::item(
                    IconName::Github,
                    format!("Open attached branch “{}” on GitHub", ws.branch_of(r)),
                    move |_, cx| cx.open_url(&url),
                )
            })
        }
        _ => each_or_all(IconName::Github, "Open branch on GitHub", links, open_urls),
    };
    let Some(name) = repo else {
        let pr = sync.and_then(|sync| sync.pr_entry());
        let github: Vec<Entry> = [branch_link, pr].into_iter().flatten().collect();
        if !github.is_empty() {
            entries.push(Entry::Sep);
            entries.extend(github);
        }
        return entries;
    };
    // A repository's menu leads with its pull request, the next thing to do with a branch.
    let pr_step = |icon, label: &'static str, kind: crate::sync::Kind| {
        let name = name.to_string();
        let step = on_ws(cx, &ws.name, move |s, n, w, cx| {
            if let Some(g) = s.syncs.get(&n).cloned() {
                let (kind, repos) = (kind.clone(), vec![name.clone()]);
                let label = label.trim_end_matches('…');
                g.update(cx, |g, cx| g.run_kind_in(kind, label, repos, w, cx));
            }
        });
        menu::item(icon, label, step)
    };
    let authored = scoped.filter(|r| !r.is_review());
    let create = authored
        .filter(|r| matches!(r.pr, Some(Ok(None))))
        .map(|_| {
            pr_step(
                IconName::GitPullRequestCreate,
                "Create pull request",
                crate::sync::Kind::CreatePr,
            )
        });
    let pr = scoped.and_then(|r| r.pr()).map(|p| {
        let state = match p.state {
            PrState::Open if p.draft => "Draft",
            PrState::Open => "Open",
            PrState::Merged => "Merged",
            PrState::Closed => "Closed",
        };
        let url = p.url.clone();
        menu::item(
            IconName::GitPullRequest,
            "Open pull request",
            move |_, cx| cx.open_url(&url),
        )
        .hint(format!("#{} · {state}", p.number))
    });
    let merge = authored.and_then(|r| r.open_pr()).map(|p| {
        pr_step(
            IconName::GitMerge,
            "Merge pull request…",
            crate::sync::Kind::Merge,
        )
        .hint(format!("#{}", p.number))
    });
    let mut menu: Vec<Entry> = [create, pr, merge, branch_link]
        .into_iter()
        .flatten()
        .collect();
    if !menu.is_empty() {
        menu.push(Entry::Sep);
    }
    menu.extend(entries);
    menu.push(Entry::Sep);
    let path = ws.dir().join(name);
    let reveal = path.clone();
    menu.push(menu::item(
        IconName::FolderOpen,
        "Reveal in Finder",
        move |_, cx| cx.reveal_path(&reveal),
    ));
    menu.extend(open_in_entries(None, &[(name.to_string(), path)], cx));
    menu.extend(worktree);
    let repo = name.to_string();
    menu.push(menu::item(
        IconName::Settings,
        "Repository settings…",
        on_shark(cx, move |s, w, cx| {
            if let Some(r) = s.state.repos.iter().find(|r| r.name == repo).cloned() {
                repo_settings::open(s, r, w, cx)
            }
        }),
    ));
    menu
}

/// Restack onto the parent (the merged parent's base once it merged), new workspace on top,
/// insert one below. Review checkouts get none of them.
fn stack_actions(
    ws: &Workspace,
    sync: Option<&crate::sync::GitSync>,
    repo: Option<&str>,
    attached_branch: Option<&str>,
    cx: &Context<Shark>,
) -> Vec<Entry> {
    let repos: Vec<_> = sync
        .and_then(|g| g.repos())
        .unwrap_or_default()
        .iter()
        .filter(|r| repo.is_none_or(|name| name == r.name))
        .collect();
    if repos.iter().any(|r| r.is_review()) {
        return vec![];
    }
    let name = ws.name.clone();
    let merged: Vec<String> = (repos.iter().filter(|r| r.parent_merged()))
        .map(|r| r.name.clone())
        .collect();
    let stacked: Vec<&crate::sync::Repo> = repos
        .iter()
        .copied()
        .filter(|r| r.stacked().is_some())
        .collect();
    let mut out = vec![];
    if let Some(onto) = (repos.iter().find(|r| r.parent_merged())).and_then(|r| r.parent.as_ref()) {
        let label = format!("Parent merged · rebase onto {}", onto.base);
        out.push(
            menu::item(
                IconName::Layers,
                label.clone(),
                on_ws(cx, &name, move |s, n, w, cx| {
                    if let Some(g) = s.syncs.get(&n).cloned() {
                        let (label, rs) = (label.clone(), merged.clone());
                        g.update(cx, |g, cx| {
                            g.run_kind_in(crate::sync::Kind::Restack, &label, rs, w, cx)
                        });
                    }
                }),
            )
            .hint(
                attached_branch.map_or_else(|| "Agent".into(), |b| format!("Attached branch: {b}")),
            ),
        );
    } else if let Some(r) = stacked.first() {
        let p = r.stacked().unwrap_or_default();
        let label = match stacked.len() {
            1 => format!("Rebase onto {p}"),
            _ => "Rebase onto parent".to_string(),
        };
        let behind = r.local.as_ref().map_or(0, |l| l.base.1);
        let repo = repo.map(str::to_string);
        out.push(
            menu::item(
                IconName::GitCompareArrows,
                label,
                on_ws(cx, &name, move |s, n, w, cx| {
                    if let Some(g) = s.syncs.get(&n).cloned() {
                        g.update(cx, |g, cx| g.restack_onto_parent(repo.as_deref(), w, cx));
                    }
                }),
            )
            .hint(match (attached_branch, behind) {
                (Some(b), 0) => format!("Attached branch: {b}"),
                (Some(b), n) => format!("Attached branch: {b} · {n} behind"),
                (None, 0) => "Agent".to_string(),
                (None, n) => format!("{n} behind"),
            }),
        );
    }
    if repo.is_some() {
        return out;
    }
    out.push(
        menu::item(
            IconName::Layers,
            "New agent on top…",
            on_ws(cx, &name, |s, n, w, cx| s.stack_from(n, false, w, cx)),
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
            "Insert agent below…",
            on_ws(cx, &name, |s, n, w, cx| s.stack_from(n, true, w, cx)),
        )
        .hint(below),
    );
    out
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

/// Right-click menu of a sidebar repo row.
pub(super) fn repo_menu(this: &Shark, repo: &Repo, cx: &Context<Shark>) -> Vec<Entry> {
    let (name, path) = (repo.name.clone(), repo.path.clone());
    let (settings, picked, p1, p3) = (repo.clone(), vec![name.clone()], path.clone(), path.clone());
    let mut entries = vec![
        menu::item(
            IconName::Settings,
            "Repository settings…",
            on_shark(cx, move |s, w, cx| {
                repo_settings::open(s, settings.clone(), w, cx)
            }),
        ),
        menu::item(
            IconName::Plus,
            "New agent from this repo…",
            on_shark(cx, move |s, w, cx| {
                s.new_workspace(picked.clone(), None, w, cx)
            }),
        ),
        Entry::Sep,
        menu::item(IconName::FolderOpen, "Reveal in Finder", move |_, cx| {
            cx.reveal_path(&p1)
        })
        .hint(tilde(&path)),
    ];
    // Active workspace first: the likeliest target.
    let mut targets: Vec<&Workspace> = (this.state.workspaces.iter())
        .filter(|w| !w.repos.iter().any(|r| r.name == name))
        .collect();
    targets.sort_by_key(|w| this.active.as_ref() != Some(&w.name));
    let rows: Vec<Entry> = (targets.into_iter())
        .map(|w| {
            let (ws, repo) = (w.name.clone(), repo.clone());
            menu::item(
                IconName::MessageSquare,
                w.display_title().to_string(),
                on_shark(cx, move |s, win, cx| {
                    s.attach_form(ws.clone(), vec![repo.name.clone()], win, cx)
                }),
            )
        })
        .collect();
    if !rows.is_empty() {
        entries.insert(2, menu::sub(IconName::FolderPlus, "Attach to agent", rows));
    }
    entries.extend(open_in_entries(None, &[(name, path.clone())], cx));
    entries.push(
        menu::item(IconName::Copy, "Copy path", move |w, cx| {
            copy(p3.display().to_string(), w, cx)
        })
        .hint(tilde(&path)),
    );
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

/// `on_shark` that hands `f` its own copy of the workspace `name`.
pub(super) fn on_ws(
    cx: &Context<Shark>,
    name: &str,
    f: impl Fn(&mut Shark, String, &mut Window, &mut Context<Shark>) + 'static,
) -> impl Fn(&mut Window, &mut App) + 'static {
    let name = name.to_string();
    on_shark(cx, move |s, w, cx| f(s, name.clone(), w, cx))
}
