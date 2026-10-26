//! PR stacks: a branch based on another branch with an open PR, as `main ← logic ← ui`.
//! Pure, from the local workspaces and a repo's open PRs; nothing about stacks is stored.
use crate::complete::Pr;

/// One branch of a stack: its open PR, the local workspace on it, either or both.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub branch: String,
    pub pr: Option<Pr>,
    pub ws: Option<String>,
}

/// The branch a base like `origin/logic` stacks on; `None` for the trunk, a review
/// checkout's `refs/shark/…` base, or anything else not on origin.
pub fn parent_of<'a>(base: &'a str, trunk: &str) -> Option<&'a str> {
    (base != trunk)
        .then(|| base.strip_prefix("origin/"))
        .flatten()
        .filter(|p| !p.is_empty())
}

/// `me`'s stack in one repo, trunk side first, and `me`'s index in it. `local` is
/// `(workspace, branch, base)` for every workspace with this repo; `prs` are its open PRs.
/// The chain ends at the trunk, at a branch nothing more is known about, or where it would
/// loop. ponytail: one child per branch (a workspace before a PR, then the newest PR); list
/// siblings if branching stacks show up.
pub fn chain(
    me: &str,
    trunk: &str,
    local: &[(String, String, String)],
    prs: &[Pr],
) -> (Vec<Node>, usize) {
    let trunk_name = trunk.strip_prefix("origin/").unwrap_or(trunk);
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
    // workspace's base before its PR's) says is on `b`, so walking up and down agree.
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

/// The header's label: `logic ← ui` when every stacked repo has the same parent, else the branch.
pub fn title(branch: &str, parents: &[&str]) -> String {
    let mut ps = parents.to_vec();
    ps.sort();
    ps.dedup();
    match ps[..] {
        [p] => format!("{p} ← {branch}"),
        _ => branch.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn ws(name: &str, branch: &str, base: &str) -> (String, String, String) {
        (name.into(), branch.into(), base.into())
    }

    fn names(c: &(Vec<Node>, usize)) -> (Vec<&str>, usize) {
        (c.0.iter().map(|n| n.branch.as_str()).collect(), c.1)
    }

    #[test]
    fn parent_of_skips_trunk_reviews_and_local_refs() {
        assert_eq!(parent_of("origin/logic", "origin/main"), Some("logic"));
        assert_eq!(parent_of("origin/main", "origin/main"), None);
        assert_eq!(parent_of("refs/shark/pull/7/base", "origin/main"), None);
        assert_eq!(parent_of("main", "main"), None);
        assert_eq!(parent_of("origin/", "origin/main"), None);
    }

    /// Workspaces and PRs mix: logic is a workspace, ui a workspace on it, wiring only a PR.
    #[test]
    fn chain_walks_workspaces_and_prs() {
        let local = [
            ws("logic", "logic", "origin/main"),
            ws("ui", "ui", "origin/logic"),
            ws("backend", "be", "origin/main"),
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

    /// A colleague's PR stack, with no workspace for any of it but ours.
    #[test]
    fn chain_from_prs_alone() {
        let local = [ws("mine", "mine", "origin/b")];
        let prs = [pr(1, "a", "main"), pr(2, "b", "a")];
        assert_eq!(
            names(&chain("mine", "origin/main", &local, &prs)),
            (vec!["a", "b", "mine"], 2)
        );
    }

    /// A merged parent is gone from the open PRs: it ends the chain by name.
    #[test]
    fn chain_ends_at_an_unknown_parent() {
        let local = [ws("ui", "ui", "origin/logic")];
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
        let local = [ws("a", "a", "origin/b"), ws("b", "b", "origin/a")];
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

    /// One child per branch: a workspace before a PR, then the newest PR. Fork PRs never count.
    #[test]
    fn chain_picks_one_child() {
        let local = [ws("a", "a", "origin/main"), ws("c", "c", "origin/a")];
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

    /// A workspace on the trunk isn't the bottom of every PR's stack; a workspace's own base
    /// outranks its PR's stale one, from either end of the chain.
    #[test]
    fn chain_skips_the_trunk_and_trusts_workspace_bases() {
        let prs = [pr(1, "feature", "main")];
        let local = [ws("main", "main", "origin/main")];
        assert_eq!(names(&chain("main", "origin/main", &local, &prs)), (vec!["main"], 0));
        // b was retargeted from a to main; its PR hasn't caught up yet.
        let local = [ws("a", "a", "origin/main"), ws("b", "b", "origin/main")];
        let prs = [pr(1, "a", "main"), pr(2, "b", "a")];
        assert_eq!(names(&chain("a", "origin/main", &local, &prs)), (vec!["a"], 0));
        assert_eq!(names(&chain("b", "origin/main", &local, &prs)), (vec!["b"], 0));
    }

    #[test]
    fn title_shows_one_shared_parent() {
        assert_eq!(title("ui", &["logic"]), "logic ← ui");
        assert_eq!(title("ui", &["logic", "logic"]), "logic ← ui");
        assert_eq!(title("ui", &["logic", "api-x"]), "ui");
        assert_eq!(title("ui", &[]), "ui");
    }
}
