//! Prompts the git button sends to the agent. Users can override each one in Settings;
//! only overrides are saved, so improving a default reaches everyone who didn't edit it.
use std::collections::BTreeMap;
use std::sync::RwLock;

pub struct Prompt {
    pub id: &'static str,
    pub title: &'static str,
    /// What `{repos}` expands to, shown under the editor.
    pub repos: &'static str,
    pub default: &'static str,
}

pub const PROMPTS: &[Prompt] = &[
    Prompt {
        id: "commit",
        title: "Commit and push",
        repos: "repos selected for committing (may still be clean when queued)",
        default: "Commit and push changes in: {repos}. Check the current git state first; skip repos with nothing to commit or push. \
In each repo review the diff and stage the files that \
belong to this change. Never commit secrets or build output; if such files show up, add them to \
.gitignore instead. Write a concise commit message in the repo's style, commit, then \
`git push -u origin {branch}:{origin_branch}`. If the push is rejected because the remote moved, \
`git pull --rebase origin {origin_branch}` and push again.",
    },
    Prompt {
        id: "conflicts",
        title: "Resolve conflicts",
        repos: "repos with unresolved conflicts",
        default: "These repos have unresolved conflicts: {repos}. In each, inspect `git status`, resolve every conflict \
keeping the intent of both sides, then finish the merge or rebase in progress \
(`git rebase --continue` or `git commit`). Abort only if resolving is impossible, and explain why.",
    },
    Prompt {
        id: "finish",
        title: "Finish operation",
        repos: "repos with an unfinished merge, rebase, cherry-pick or revert",
        default: "A merge, rebase, cherry-pick or revert was left unfinished in: {repos}. Check `git status` in each and \
finish it (`git rebase --continue`, `git commit`, …), then report what you did.",
    },
    Prompt {
        id: "detached",
        title: "Fix detached HEAD",
        repos: "repos with a detached HEAD",
        default: "HEAD is detached in: {repos}. Get back onto branch `{branch}` without losing commits \
(if HEAD has commits that aren't on `{branch}`, bring them over), then report what you did.",
    },
    Prompt {
        id: "wrong_branch",
        title: "Switch to branch",
        repos: "repos on another branch",
        default: "These repos are on a different branch than their task branch `{branch}`: {repos}. Switch them back to \
`{branch}` (bring over any commits made on the other branch), then report what you did.",
    },
    Prompt {
        id: "diverged",
        title: "Sync with remote",
        repos: "repos whose branch diverged from origin",
        default: "Branch `{branch}` has diverged from `origin/{origin_branch}` in: {repos}. Rebase the local commits onto \
`origin/{origin_branch}` (`git pull --rebase origin {origin_branch}`), resolve any conflicts, run a quick sanity \
check, and `git push origin {branch}:{origin_branch}`.",
    },
    Prompt {
        id: "pr_conflicts",
        title: "Resolve PR conflicts",
        repos: "repos whose PR conflicts, with their base",
        default: "These PRs conflict with their base: {repos}. In each repo `git fetch origin`, merge the base into \
`{branch}`, resolve the conflicts, run a quick sanity check, commit and push.",
    },
    Prompt {
        id: "checks",
        title: "Fix checks",
        repos: "repos with failing checks, and the check names",
        default: "CI checks fail on the PRs ({repos}). For each, read the failure (`gh pr checks`, then \
`gh run view <run-id> --log-failed` in that repo), fix the cause, commit and push. \
Don't disable or skip checks.",
    },
    Prompt {
        id: "review",
        title: "Address review",
        repos: "repos whose PR has changes requested",
        default: "Reviewers requested changes on the PRs in: {repos}. Read the review comments \
(`gh pr view --comments` and `gh api repos/{owner}/{repo}/pulls/<n>/comments`), address each one, \
commit and push, then summarize what you changed per comment.",
    },
    Prompt {
        id: "comments",
        title: "Address comments",
        repos: "repos whose PR has new review comments",
        default: "Reviewers left comments on the PRs in: {repos}. Read them (`gh pr view --comments` and \
`gh api repos/{owner}/{repo}/pulls/<n>/comments`). Address the ones that ask for a change, \
commit and push, then summarize per comment what you changed or why you left it.",
    },
    Prompt {
        id: "update_base",
        title: "Update from base",
        repos: "every repo, with its base",
        default: "Bring the latest base branch into `{branch}` in every repo that is behind it: `git fetch origin`, \
merge the base ({repos}), resolve conflicts, run a quick sanity check, commit and push.",
    },
    Prompt {
        id: "rebase_base",
        title: "Rebase onto new base",
        repos: "the repos whose base changed, old → new",
        default: "The base of `{branch}` changed ({repos}). In each of those repos: `git fetch origin`, \
then `git rebase --onto <new> $(git merge-base HEAD <old>)` so only this branch's own commits move. \
Resolve conflicts keeping this branch's intent, run a quick sanity check, then \
`git push --force-with-lease origin {branch}:{origin_branch}` if the branch is on origin.",
    },
    Prompt {
        id: "restack",
        title: "Restack onto parent",
        repos: "the stacked repos: where their commits go, and the parent commit they start after",
        default: "Restack `{branch}` so it holds only its own commits on top of its parent ({repos}). The commits up to \
the given one belong to the parent, which may have new commits, been rewritten or been squash-merged since. In each of \
those repos: `git fetch origin`, then `git rebase --onto <onto> <after>`. Resolve conflicts keeping this branch's \
intent, check that `git log --oneline <onto>..HEAD` lists only this branch's own commits, run a quick sanity check, \
then `git push --force-with-lease origin {branch}:{origin_branch}` if the branch is on origin.",
    },
];

/// The user's edits, by id. A global because `next_action` is pure and runs off the main thread.
static OVERRIDES: RwLock<BTreeMap<String, String>> = RwLock::new(BTreeMap::new());

pub fn set_overrides(o: &BTreeMap<String, String>) {
    *OVERRIDES.write().unwrap() = o.clone();
}

pub fn get(id: &str) -> String {
    // A blank override (saved by an older version) would queue a message that never sends.
    if let Some(p) = OVERRIDES
        .read()
        .unwrap()
        .get(id)
        .filter(|p| !p.trim().is_empty())
    {
        return p.clone();
    }
    PROMPTS
        .iter()
        .find(|p| p.id == id)
        .expect(id)
        .default
        .into()
}

/// Fills `{repos}`, `{branch}` and `{origin_branch}`, the branch's name on origin
/// (`git::origin_branch`); other braces (gh's `{owner}`) stay as written.
pub fn render(id: &str, repos: &str, branch: &str, on_origin: &str) -> String {
    get(id)
        .replace("{repos}", repos)
        .replace("{branch}", branch)
        .replace("{origin_branch}", on_origin)
}

/// `render` for `(repo, branch, its name on origin)`, once per branch they're on: the names sit
/// inside git commands, so each must be one. `fill` gives `{repos}` for each group's repos.
pub fn render_each<'a>(
    id: &str,
    repos: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>,
    fill: impl Fn(&[String]) -> String,
) -> String {
    let mut groups: Vec<((&str, &str), Vec<String>)> = vec![];
    for (r, b, on_origin) in repos {
        match groups.iter_mut().find(|(g, _)| *g == (b, on_origin)) {
            Some((_, rs)) => rs.push(r.into()),
            None => groups.push(((b, on_origin), vec![r.into()])),
        }
    }
    (groups.iter())
        .map(|((b, on_origin), rs)| render(id, &fill(rs), b, on_origin))
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    /// Overrides are covered by `settings_apply_and_save`, under the UI tests' lock;
    /// the global is shared with them, so this test only reads an id they never edit.
    #[test]
    fn fills_repos_and_branch() {
        let p = super::render("review", "api", "b", "b");
        assert!(p.contains("in: api.") && p.contains("repos/{owner}/{repo}"));
        let p = super::render("detached", "api, web", "feat", "feat");
        assert!(p.starts_with("HEAD is detached in: api, web.") && p.contains("`feat`"));
        let feat = [("api", "feat", "feat"), ("web", "feat", "feat")];
        let one = super::render_each("detached", feat, |rs| rs.join(", "));
        assert_eq!(one, p, "one branch, one prompt");
        let two = super::render_each(
            "detached",
            [("api", "a", "a"), ("web", "b", "b"), ("cli", "a", "a")],
            |rs| rs.join(", "),
        );
        let (a, b) = two.split_once("\n\n").unwrap();
        assert!(
            a.starts_with("HEAD is detached in: api, cli.") && a.contains("`a`"),
            "{a}"
        );
        assert!(
            b.starts_with("HEAD is detached in: web.") && b.contains("`b`") && !b.contains("`a`")
        );
        // A branch origin knows by another name is pushed and pulled under that name.
        let join = |rs: &[String]| rs.join(", ");
        let p = super::render_each("commit", [("api", "feat", "kd/feat")], join);
        assert!(p.contains("`git push -u origin feat:kd/feat`"), "{p}");
        assert!(p.contains("`git pull --rebase origin kd/feat`"), "{p}");
        let named = [("api", "feat", "kd/feat"), ("web", "feat", "feat")];
        let two = super::render_each("diverged", named, join);
        let (a, b) = two
            .split_once("\n\n")
            .expect("one prompt per name on origin");
        assert!(
            a.starts_with("Branch `feat` has diverged from `origin/kd/feat` in: api."),
            "{a}"
        );
        assert!(a.contains("`git push origin feat:kd/feat`"), "{a}");
        assert!(b.contains("`origin/feat` in: web."), "{b}");
        for id in ["rebase_base", "restack"] {
            let p = super::render(id, "api", "feat", "kd/feat");
            let push = "`git push --force-with-lease origin feat:kd/feat`";
            assert!(p.contains(push), "{id}: {p}");
        }
    }
}
