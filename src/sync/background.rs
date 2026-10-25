//! A slow check of workspaces that aren't on screen, so their sidebar rows can say "needs you"
//! (red CI, a review, conflicts) without opening them. It never fetches: that would fight the
//! agent for refs, and what changes while nobody looks is GitHub's answer.
use super::*;

/// Blocking: one `git status` and one `gh pr list` per repo, in parallel, each with its timeout.
/// `Ok(label)` names the step when it is the user's to take; `Err` when gh failed for any repo
/// (offline, logged out, rate limited), so the caller keeps the last answer and may back off.
pub fn check(ws: &Workspace) -> Result<Option<String>, String> {
    let job = Job {
        branch: ws.branch.clone(),
        repos: worktrees(ws),
        fetch: false,
        pr: true,
        prev: vec![],
    };
    let p = run_job(&job);
    let prs = p.prs.unwrap_or_default();
    if let Some(Err(e)) = prs.iter().find(|p| p.is_err()) {
        return Err(e.clone());
    }
    let repos = repos_from(ws, p.locals, prs.into_iter().map(Some));
    let a = next_action(&repos, &ws.branch, &crate::state::reviewed(&ws.name));
    Ok(needs_you(&a).then_some(a.label))
}

/// Something is wrong (conflicts, failing checks, a git error), a review waits, or it can merge.
fn needs_you(a: &Action) -> bool {
    a.attention || !a.reviews.is_empty() || a.kind == Kind::Merge
}

impl GitSync {
    /// The shown workspace's own answer, from its faster polling.
    pub fn needs_you(&self) -> Option<String> {
        self.action
            .as_ref()
            .filter(|a| needs_you(a))
            .map(|a| a.label.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, Kind, needs_you};

    #[test]
    fn what_needs_you() {
        let a = |kind, attention, reviews: &[&str]| Action {
            label: String::new(),
            kind,
            repos: vec![],
            attention,
            reviews: reviews.iter().map(|s| s.to_string()).collect(),
        };
        assert!(needs_you(&a(Kind::Merge, false, &[])));
        assert!(needs_you(&a(Kind::Agent("fix".into()), true, &[])));
        assert!(needs_you(&a(Kind::Agent("review".into()), false, &["r1"])));
        assert!(!needs_you(&a(Kind::Agent("commit".into()), false, &[])));
        assert!(!needs_you(&a(Kind::OpenPr, false, &[])), "a wait");
    }
}
