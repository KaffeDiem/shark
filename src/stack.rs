//! PR stacks: a branch based on another branch with an open PR, as `main ← logic ← ui`.
//! Pure, from the local tasks and a repo's open PRs; nothing about stacks is stored.
use crate::prs::Pr;

/// One branch of a stack: its open PR, the local task on it, either or both.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub branch: String,
    pub pr: Option<Pr>,
    pub ws: Option<String>,
}

/// The branch a base stacks on: `logic` for `origin/logic`, and for the bare `logic` of a task
/// cut from a branch that isn't pushed yet. `None` for the trunk and for a review checkout's
/// `refs/shark/…` base.
pub fn parent_of<'a>(base: &'a str, trunk: &str) -> Option<&'a str> {
    match base.strip_prefix("origin/") {
        Some(parent) => (!parent.is_empty() && base != trunk).then_some(parent),
        // A local branch, and only with the trunk known: its own local branch is no parent.
        None => (!base.is_empty()
            && !trunk.is_empty()
            && base != "HEAD"
            && !base.starts_with("refs/")
            && base != crate::git::short(trunk))
        .then_some(base),
    }
}

/// `branch`'s open PR among `prs`: not a fork's of the same name, and none when two disagree.
pub fn own_pr<'a>(prs: &'a [Pr], branch: &str) -> Option<&'a Pr> {
    let mut own = (prs.iter()).filter(|p| !p.is_cross_repository && p.head_ref_name == branch);
    let first = own.next()?;
    own.all(|p| p.base_ref_name == first.base_ref_name)
        .then_some(first)
}

/// Whether a task with base `saved` follows its PR to `now`, GitHub's base for it: when GitHub
/// changed it since `before` (`None`: not seen yet), so a base set here that GitHub hasn't caught
/// up with stays. Only an `origin/…` base: a review checkout's `refs/shark/…` is its GitSync's to
/// replace, and another remote's isn't GitHub's to say.
pub fn follows(saved: &str, before: Option<&str>, now: &str) -> bool {
    !now.is_empty()
        && saved.starts_with("origin/")
        && crate::git::short(saved) != now
        && before != Some(now)
}

/// `me`'s stack in one repo, trunk side first, and `me`'s index in it. `local` is
/// `(task, branch, base)` for every task with this repo; `prs` are its open PRs.
/// The chain ends at the trunk, at a branch nothing more is known about, or where it would
/// loop. ponytail: one child per branch (a task before a PR, then the newest PR); list
/// siblings if branching stacks show up.
pub fn chain(
    me: &str,
    trunk: &str,
    local: &[(String, String, String)],
    prs: &[Pr],
) -> (Vec<Node>, usize) {
    let trunk_name = crate::git::short(trunk);
    let own: Vec<&Pr> = prs.iter().filter(|p| !p.is_cross_repository).collect();
    let node = |b: &str| Node {
        branch: b.to_string(),
        pr: own
            .iter()
            .find(|p| p.head_ref_name == b)
            .map(|p| (*p).clone()),
        ws: local.iter().find(|l| l.1 == b).map(|l| l.0.clone()),
    };
    let parent = |b: &str| -> Option<String> {
        match local.iter().find(|l| l.1 == b) {
            Some(l) => parent_of(&l.2, trunk).map(String::from),
            None => (own.iter().find(|p| p.head_ref_name == b))
                .map(|p| p.base_ref_name.clone())
                .filter(|p| p != trunk_name && !p.is_empty()),
        }
    };
    // The trunk is nobody's child, so it starts no stack. A child is whatever `parent` (a
    // task's base before its PR's) says is on `b`, so walking up and down agree.
    let child = |b: &str| -> Option<String> {
        if b == trunk_name {
            return None;
        }
        let on_b = |c: &str| c != b && parent(c).as_deref() == Some(b);
        (local.iter().map(|l| &l.1).find(|c| on_b(c)).cloned()).or_else(|| {
            (own.iter())
                .filter(|p| on_b(&p.head_ref_name))
                .max_by(|a, b| a.updated_at.cmp(&b.updated_at))
                .map(|p| p.head_ref_name.clone())
        })
    };
    let mut seen = vec![me.to_string()];
    let mut down = vec![];
    let mut at = me.to_string();
    while let Some(p) = parent(&at).filter(|p| !seen.contains(p)) {
        seen.push(p.clone());
        down.push(node(&p));
        at = p;
    }
    down.reverse();
    let ix = down.len();
    down.push(node(me));
    let mut at = me.to_string();
    while let Some(c) = child(&at).filter(|c| !seen.contains(c)) {
        seen.push(c.clone());
        down.push(node(&c));
        at = c;
    }
    (down, ix)
}

/// A stack as the sidebar draws it: one repo's `chain`, trunk side first, ending in `base`.
#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    pub repo: String,
    /// Each has a task or an open PR.
    pub nodes: Vec<Node>,
    /// What the bottom node lands on, e.g. `main`; a fork's is the branch it shares.
    pub base: String,
}

/// A sidebar task for [`columns`]: its name and `(repo, branch, base)` per repo.
pub type Listed<'a> = (&'a str, Vec<(&'a str, &'a str, &'a str)>);

/// The sidebar's stacks. `tasks` in sidebar order; a task joins one stack, in its first repo
/// (of `only`, when set) with one. The chain from the bottom claims shared parents before a
/// fork does; a fork keeps what is above them and lands on the last shared branch. A lone task
/// counts only off the trunk, as on `release-2`.
pub fn columns<'a>(
    tasks: &[Listed],
    trunk: impl Fn(&str) -> Option<&'a str>,
    prs: impl Fn(&str) -> &'a [Pr],
    only: Option<&str>,
) -> Vec<Column> {
    use std::collections::{HashMap, HashSet};
    /// What `chain` needs of one repo, and the branches something is stacked on.
    struct Repo<'a> {
        trunk: &'a str,
        local: Vec<(String, String, String)>,
        open: &'a [Pr],
        parents: HashSet<String>,
    }
    let mut by_repo: HashMap<&str, Repo> = HashMap::new();
    for (_, repos) in tasks {
        for &(repo, ..) in repos {
            if by_repo.contains_key(repo) || only.is_some_and(|o| o != repo) {
                continue;
            }
            let Some(t) = trunk(repo).filter(|t| !t.is_empty()) else {
                continue;
            };
            let local: Vec<(String, String, String)> = (tasks.iter())
                .filter_map(|(n, rs)| {
                    let (_, b, base) = rs.iter().find(|r| r.0 == repo)?;
                    Some((n.to_string(), b.to_string(), base.to_string()))
                })
                .collect();
            let open = prs(repo);
            let parents = (local.iter().filter_map(|l| parent_of(&l.2, t)))
                .chain(
                    (open.iter().filter(|p| !p.is_cross_repository))
                        .map(|p| p.base_ref_name.as_str()),
                )
                .map(String::from)
                .collect();
            let r = Repo {
                trunk: t,
                local,
                open,
                parents,
            };
            by_repo.insert(repo, r);
        }
    }
    // Each task's stack is in its first repo where it has a parent or a child. Most tasks
    // are on the trunk with nothing on them: they have none, and no chain is walked.
    let home: HashMap<&str, (&str, &str)> = (tasks.iter())
        .filter_map(|(name, repos)| {
            let (repo, branch, _) = repos.iter().find(|&&(repo, branch, base)| {
                by_repo.get(repo).is_some_and(|r| {
                    parent_of(base, r.trunk).is_some() || r.parents.contains(branch)
                })
            })?;
            Some((*name, (*repo, *branch)))
        })
        .collect();
    let mut taken: HashSet<String> = HashSet::new();
    let mut out = vec![];
    for (name, _) in tasks {
        let Some(&(repo, branch)) = home.get(name).filter(|_| !taken.contains(*name)) else {
            continue;
        };
        let Repo {
            trunk: t,
            local,
            open,
            ..
        } = &by_repo[repo];
        let (nodes, _) = chain(branch, t, local, open);
        if nodes.len() < 2 {
            continue;
        }
        // A task is free here until a stack takes it, and only in its own repo.
        let free = |n: &Node, taken: &HashSet<String>| {
            n.ws.as_ref().is_none_or(|w| {
                !taken.contains(w) && home.get(w.as_str()).is_some_and(|h| h.0 == repo)
            })
        };
        let base_of = |n: &Node| match (&n.ws, &n.pr) {
            (Some(w), _) => (local.iter().find(|l| &l.0 == w))
                .map(|l| crate::git::short(&l.2).to_string())
                .unwrap_or_default(),
            (None, Some(p)) => p.base_ref_name.clone(),
            (None, None) => n.branch.clone(),
        };
        let mut claim = |nodes: &[Node], at: usize, taken: &mut HashSet<String>| {
            let (mut lo, mut hi) = (at, at);
            while lo > 0 && free(&nodes[lo - 1], taken) {
                lo -= 1;
            }
            while hi + 1 < nodes.len() && free(&nodes[hi + 1], taken) {
                hi += 1;
            }
            // A branch with neither task nor open PR ends the chain and is its base.
            let unknown = |n: &Node| n.ws.is_none() && n.pr.is_none();
            let (lo, base) = match lo {
                0 if unknown(&nodes[0]) => (1, nodes[0].branch.clone()),
                0 => (0, base_of(&nodes[0])),
                _ => (lo, nodes[lo - 1].branch.clone()),
            };
            let run = &nodes[lo..=hi];
            if run.iter().all(|n| n.ws.is_none()) || (run.len() < 2 && base == crate::git::short(t))
            {
                return;
            }
            taken.extend(run.iter().filter_map(|n| n.ws.clone()));
            out.push(Column {
                repo: repo.to_string(),
                nodes: run.to_vec(),
                base,
            });
        };
        // The chain from the bottom claims shared parents before a fork does.
        let me = |ns: &[Node]| ns.iter().position(|n| n.ws.as_deref() == Some(*name));
        if nodes[0].ws.as_deref() != Some(*name) {
            let (primary, _) = chain(&nodes[0].branch, t, local, open);
            let first = primary
                .iter()
                .position(|n| n.ws.is_some() && free(n, &taken));
            if let (None, Some(at)) = (me(&primary), first) {
                claim(&primary, at, &mut taken);
            }
        }
        if let Some(at) = me(&nodes) {
            claim(&nodes, at, &mut taken);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols<'a>(
        tasks: &'a [(&'a str, &'a str, &'a str)],
        prs: &[Pr],
    ) -> Vec<(Vec<String>, String)> {
        let listed: Vec<Listed> = (tasks.iter())
            .map(|(n, b, base)| (*n, vec![("api", *b, *base)]))
            .collect();
        columns(&listed, |_| Some("origin/main"), |_| prs, None)
            .into_iter()
            .map(|c| (c.nodes.into_iter().map(|n| n.branch).collect(), c.base))
            .collect()
    }

    fn col(nodes: &[&str], base: &str) -> (Vec<String>, String) {
        (nodes.iter().map(|n| n.to_string()).collect(), base.into())
    }

    /// One stack per chain, whichever member comes first; ordinary tasks get none.
    #[test]
    fn columns_group_a_chain_once() {
        let tasks = [
            ("ui", "ui", "origin/api"),
            ("plain", "plain", "origin/main"),
            ("api", "api", "origin/main"),
            ("wiring", "wiring", "origin/ui"),
        ];
        assert_eq!(cols(&tasks, &[]), [col(&["api", "ui", "wiring"], "main")]);
    }

    /// A lone task off the trunk is a stack; an unknown parent becomes the base row.
    #[test]
    fn columns_show_a_non_trunk_base() {
        let tasks = [("hotfix", "hotfix", "origin/release-2")];
        assert_eq!(cols(&tasks, &[]), [col(&["hotfix"], "release-2")]);
        let tasks = [("lone", "lone", "origin/main")];
        assert!(cols(&tasks, &[]).is_empty());
    }

    /// A colleague's PR between two tasks joins them; the base is the bottom PR's.
    #[test]
    fn columns_keep_pr_rows() {
        let tasks = [("ui", "ui", "origin/models"), ("api", "api", "origin/main")];
        let prs = [pr(7, "models", "api")];
        assert_eq!(cols(&tasks, &prs), [col(&["api", "models", "ui"], "main")]);
        let tasks = [("ui", "ui", "origin/models")];
        let prs = [pr(7, "models", "release")];
        assert_eq!(cols(&tasks, &prs), [col(&["models", "ui"], "release")]);
    }

    /// Two tasks on one parent: the first child keeps the parents, the second lands on
    /// the shared branch, whichever is listed first.
    #[test]
    fn columns_split_forks() {
        let tasks = [
            ("emails", "emails", "origin/api"),
            ("api", "api", "origin/main"),
            ("wiring", "wiring", "origin/api"),
        ];
        // `chain` follows the first local child: emails.
        assert_eq!(
            cols(&tasks, &[]),
            [col(&["api", "emails"], "main"), col(&["wiring"], "api")]
        );
        let tasks = [tasks[2], tasks[1], tasks[0]];
        assert_eq!(
            cols(&tasks, &[]),
            [col(&["api", "wiring"], "main"), col(&["emails"], "api")]
        );
    }

    /// A task stacked in two repos shows in its first; the filter picks the repo.
    #[test]
    fn columns_pick_the_first_stacked_repo() {
        let listed: Vec<Listed> = vec![
            (
                "both",
                vec![
                    ("ios", "both", "origin/main"),
                    ("api", "both", "origin/base"),
                ],
            ),
            ("base", vec![("api", "base", "origin/main")]),
            ("top", vec![("ios", "top", "origin/both")]),
        ];
        let names = |only| {
            columns(&listed, |_| Some("origin/main"), |_| &[], only)
                .into_iter()
                .map(|c| (c.repo, c.nodes.len()))
                .collect::<Vec<_>>()
        };
        assert_eq!(names(None), [("ios".to_string(), 2)]);
        assert_eq!(names(Some("api")), [("api".to_string(), 2)]);
        // Listing order doesn't change it: base, listed first, doesn't claim both for api.
        let reordered = vec![listed[1].clone(), listed[0].clone(), listed[2].clone()];
        let got: Vec<_> = columns(&reordered, |_| Some("origin/main"), |_| &[], None)
            .into_iter()
            .map(|c| {
                (
                    c.repo,
                    c.nodes.into_iter().map(|n| n.branch).collect::<Vec<_>>(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [(
                "ios".to_string(),
                vec!["both".to_string(), "top".to_string()]
            )]
        );
        // An unknown trunk shows no stack in that repo.
        let none = columns(&listed, |_| None, |_| &[], None);
        assert!(none.is_empty());
    }

    fn pr(n: u64, head: &str, base: &str) -> Pr {
        Pr {
            repo: "ios".into(),
            number: n,
            title: format!("PR {n}"),
            author: None,
            head_ref_name: head.into(),
            base_ref_name: base.into(),
            url: format!("https://github.com/o/ios/pull/{n}"),
            state: "OPEN".into(),
            is_draft: false,
            updated_at: format!("2026-10-{n:02}"),
            is_cross_repository: false,
        }
    }

    fn task(name: &str, branch: &str, base: &str) -> (String, String, String) {
        (name.into(), branch.into(), base.into())
    }

    fn names(c: &(Vec<Node>, usize)) -> (Vec<&str>, usize) {
        (c.0.iter().map(|n| n.branch.as_str()).collect(), c.1)
    }

    #[test]
    fn parent_of_skips_the_trunk_and_review_refs() {
        assert_eq!(parent_of("origin/logic", "origin/main"), Some("logic"));
        assert_eq!(parent_of("origin/main", "origin/main"), None);
        assert_eq!(parent_of("refs/shark/pull/7/base", "origin/main"), None);
        assert_eq!(parent_of("main", "main"), None);
        assert_eq!(parent_of("origin/", "origin/main"), None);
    }

    /// A task started from another task's unpushed branch has that bare branch as its base.
    #[test]
    fn parent_of_takes_a_local_branch() {
        assert_eq!(parent_of("logic", "origin/main"), Some("logic"));
        assert_eq!(
            parent_of("kasper/abc-1-logic", "origin/main"),
            Some("kasper/abc-1-logic")
        );
        // The trunk's own local branch is still the trunk.
        assert_eq!(parent_of("main", "origin/main"), None);
        assert_eq!(parent_of("", "origin/main"), None);
        assert_eq!(parent_of("HEAD", "origin/main"), None, "no branch at all");
        // Before a repo's trunk is known, a local base may be the trunk itself.
        assert_eq!(parent_of("logic", ""), None);
        assert_eq!(
            parent_of("origin/logic", ""),
            Some("logic"),
            "as it always was"
        );
    }

    /// Workspaces and PRs mix: logic is a task, ui a task on it, wiring only a PR.
    #[test]
    fn chain_walks_tasks_and_prs() {
        let local = [
            task("logic", "logic", "origin/main"),
            task("ui", "ui", "origin/logic"),
            task("backend", "be", "origin/main"),
        ];
        let prs = [
            pr(1, "logic", "main"),
            pr(2, "ui", "logic"),
            pr(3, "wiring", "ui"),
        ];
        let c = chain("ui", "origin/main", &local, &prs);
        assert_eq!(names(&c), (vec!["logic", "ui", "wiring"], 1));
        assert_eq!(c.0[0].ws.as_deref(), Some("logic"));
        assert_eq!(c.0[0].pr.as_ref().map(|p| p.number), Some(1));
        assert_eq!(c.0[2].ws, None, "only on GitHub");
        assert_eq!(
            names(&chain("wiring", "origin/main", &local, &prs)),
            (vec!["logic", "ui", "wiring"], 2)
        );
        assert_eq!(
            names(&chain("be", "origin/main", &local, &prs)),
            (vec!["be"], 0)
        );
    }

    /// A colleague's PR stack, with no task for any of it but ours.
    #[test]
    fn chain_from_prs_alone() {
        let local = [task("mine", "mine", "origin/b")];
        let prs = [pr(1, "a", "main"), pr(2, "b", "a")];
        assert_eq!(
            names(&chain("mine", "origin/main", &local, &prs)),
            (vec!["a", "b", "mine"], 2)
        );
    }

    /// A merged parent is gone from the open PRs: it ends the chain by name.
    #[test]
    fn chain_ends_at_an_unknown_parent() {
        let local = [task("ui", "ui", "origin/logic")];
        let c = chain("ui", "origin/main", &local, &[]);
        assert_eq!(names(&c), (vec!["logic", "ui"], 1));
        assert_eq!(
            c.0[0],
            Node {
                branch: "logic".into(),
                pr: None,
                ws: None
            }
        );
    }

    #[test]
    fn chain_stops_on_loops() {
        let local = [task("a", "a", "origin/b"), task("b", "b", "origin/a")];
        assert_eq!(
            names(&chain("a", "origin/main", &local, &[])),
            (vec!["b", "a"], 1)
        );
        let prs = [pr(1, "x", "y"), pr(2, "y", "x")];
        assert_eq!(
            names(&chain("x", "origin/main", &[], &prs)),
            (vec!["y", "x"], 1)
        );
    }

    /// One child per branch: a task before a PR, then the newest PR. Fork PRs never count.
    #[test]
    fn chain_picks_one_child() {
        let local = [task("a", "a", "origin/main"), task("c", "c", "origin/a")];
        let prs = [pr(5, "b", "a"), pr(6, "d", "a")];
        assert_eq!(
            names(&chain("a", "origin/main", &local, &prs)),
            (vec!["a", "c"], 0)
        );
        assert_eq!(
            names(&chain("a", "origin/main", &local[..1], &prs)),
            (vec!["a", "d"], 0)
        );
        let mut fork = pr(7, "a", "main");
        fork.is_cross_repository = true;
        let mut fork_child = pr(8, "e", "z");
        fork_child.is_cross_repository = true;
        let c = chain("z", "origin/main", &[], &[fork, fork_child]);
        assert_eq!(names(&c), (vec!["z"], 0));
        assert_eq!(c.0[0].pr, None);
    }

    /// A task on the trunk isn't the bottom of every PR's stack; a task's own base
    /// outranks its PR's stale one, from either end of the chain.
    #[test]
    fn chain_skips_the_trunk_and_trusts_task_bases() {
        let prs = [pr(1, "feature", "main")];
        let local = [task("main", "main", "origin/main")];
        assert_eq!(
            names(&chain("main", "origin/main", &local, &prs)),
            (vec!["main"], 0)
        );
        // b was retargeted from a to main; its PR hasn't caught up yet.
        let local = [task("a", "a", "origin/main"), task("b", "b", "origin/main")];
        let prs = [pr(1, "a", "main"), pr(2, "b", "a")];
        assert_eq!(
            names(&chain("a", "origin/main", &local, &prs)),
            (vec!["a"], 0)
        );
        assert_eq!(
            names(&chain("b", "origin/main", &local, &prs)),
            (vec!["b"], 0)
        );
    }

    /// A task follows GitHub only on news: a base GitHub changed, not one it hasn't caught up
    /// with, and never a review checkout's private base.
    #[test]
    fn tasks_follow_only_a_retarget_on_github() {
        assert!(
            follows("origin/a", None, "b"),
            "first answer, already moved"
        );
        assert!(follows("origin/a", Some("a"), "b"), "retargeted");
        assert!(!follows("origin/b", Some("a"), "b"), "already there");
        assert!(
            !follows("origin/c", Some("a"), "a"),
            "set here, GitHub not there yet"
        );
        assert!(!follows("origin/a", None, ""), "no base");
        assert!(!follows("refs/shark/pull/1/base", None, "main"));
        assert!(
            !follows("upstream/main", None, "main"),
            "only origin's branches"
        );
        let prs = [pr(1, "a", "main"), pr(2, "b", "a")];
        assert_eq!(own_pr(&prs, "b").map(|p| p.number), Some(2));
        assert_eq!(own_pr(&prs, "c"), None);
        let mut fork = pr(3, "c", "main");
        fork.is_cross_repository = true;
        assert_eq!(own_pr(&[fork], "c"), None);
        let twice = [pr(1, "a", "main"), pr(4, "a", "b")];
        assert_eq!(own_pr(&twice, "a"), None, "two bases: ambiguous");
    }
}
