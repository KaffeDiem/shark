//! Shared GitHub repository identity and branch PR selection.
use crate::git::{cmd_bounded, git};
use serde_json::Value;
use std::path::Path;

/// `branch`'s PRs of any state in origin's repo (`--repo`, so a fork's upstream never answers),
/// as `gh pr list --json fields` gives them; same-named fork branches are left out.
pub fn branch_prs(wt: &Path, branch: &str, fields: &str, secs: u64) -> Result<Vec<Value>, String> {
    let origin = origin_repo(wt);
    let args = super::with_repo(
        vec![
            "pr", "list", "--head", branch, "--state", "all", "--json", fields, "--limit", "100",
        ],
        origin.as_deref(),
    );
    let json = cmd_bounded(wt, "gh", &args, secs)?;
    let list: Vec<Value> = serde_json::from_str(&json).map_err(|e| format!("gh: {e}"))?;
    Ok(own(&list).into_iter().cloned().collect())
}

/// The PRs from this repo itself; a fork's PR on a same-named branch isn't about it.
pub fn own(list: &[Value]) -> Vec<&Value> {
    list.iter()
        .filter(|p| p["isCrossRepository"] != true)
        .collect()
}

/// The open PR, else the most recently updated; also how the Git tab picks, so both agree.
pub fn pick_pr<'a>(own: &[&'a Value]) -> Option<&'a Value> {
    own.iter()
        .copied()
        .find(|p| p["state"] == "OPEN")
        .or_else(|| {
            own.iter()
                .copied()
                .max_by_key(|p| p["updatedAt"].as_str().unwrap_or("").to_string())
        })
}

/// Origin's GitHub URL, for `gh --repo`: gh may default to a fork's upstream, but branch
/// ownership belongs to origin.
pub fn origin_repo(wt: &Path) -> Option<String> {
    git(wt, &["config", "--get", "remote.origin.url"])
        .ok()
        .and_then(|url| github_url(&url))
}

/// The web URL of a GitHub remote, from its https, scp-style or ssh form.
pub fn github_url(remote: &str) -> Option<String> {
    let path = [
        "https://github.com/",
        "git@github.com:",
        "ssh://git@github.com/",
    ]
    .iter()
    .find_map(|p| remote.strip_prefix(p))?;
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    Some(format!("https://github.com/{path}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn github_urls() {
        for r in [
            "https://github.com/o/r.git",
            "https://github.com/o/r",
            "git@github.com:o/r.git",
            "ssh://git@github.com/o/r.git",
        ] {
            assert_eq!(
                super::github_url(r).as_deref(),
                Some("https://github.com/o/r"),
                "{r}"
            );
        }
        assert_eq!(super::github_url("git@gitlab.com:o/r.git"), None);
    }
}
