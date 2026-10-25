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
        repos: "repos with uncommitted changes",
        default: "Commit and push the uncommitted changes in: {repos}. In each repo review the diff and stage the files that \
belong to this change. Never commit secrets or build output; if such files show up, add them to \
.gitignore instead. Write a concise commit message in the repo's style, commit, then \
`git push -u origin {branch}`. If the push is rejected because the remote moved, \
`git pull --rebase origin {branch}` and push again.",
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
        default: "These repos are on a different branch than the workspace branch `{branch}`: {repos}. Switch them back to \
`{branch}` (bring over any commits made on the other branch), then report what you did.",
    },
    Prompt {
        id: "diverged",
        title: "Sync with remote",
        repos: "repos whose branch diverged from origin",
        default: "Branch `{branch}` has diverged from `origin/{branch}` in: {repos}. Rebase the local commits onto \
`origin/{branch}` (`git pull --rebase origin {branch}`), resolve any conflicts, run a quick sanity \
check, and push.",
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
        id: "update_base",
        title: "Update from base",
        repos: "every repo, with its base",
        default: "Bring the latest base branch into `{branch}` in every repo that is behind it: `git fetch origin`, \
merge the base ({repos}), resolve conflicts, run a quick sanity check, commit and push.",
    },
];

/// The user's edits, by id. A global because `next_action` is pure and runs off the main thread.
static OVERRIDES: RwLock<BTreeMap<String, String>> = RwLock::new(BTreeMap::new());

pub fn set_overrides(o: &BTreeMap<String, String>) {
    *OVERRIDES.write().unwrap() = o.clone();
}

pub fn get(id: &str) -> String {
    if let Some(p) = OVERRIDES.read().unwrap().get(id) {
        return p.clone();
    }
    PROMPTS
        .iter()
        .find(|p| p.id == id)
        .expect(id)
        .default
        .into()
}

/// Fills `{repos}` and `{branch}`; other braces (gh's `{owner}`) stay as written.
pub fn render(id: &str, repos: &str, branch: &str) -> String {
    get(id)
        .replace("{repos}", repos)
        .replace("{branch}", branch)
}

#[cfg(test)]
mod tests {
    #[test]
    fn override_and_fill() {
        let p = super::render("review", "api", "b");
        assert!(p.contains("in: api.") && p.contains("repos/{owner}/{repo}"));
        let o = [(
            "detached".to_string(),
            "fix {repos} on {branch}".to_string(),
        )]
        .into();
        super::set_overrides(&o);
        assert_eq!(
            super::render("detached", "api, web", "feat"),
            "fix api, web on feat"
        );
        super::set_overrides(&Default::default());
        assert!(super::render("detached", "x", "y").starts_with("HEAD is detached in: x."));
    }
}
