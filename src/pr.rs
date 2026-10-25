//! Coordinated PRs: one per repo with commits, each body linking all the others.
use crate::git::{cmd_bounded, git};
use crate::state::Workspace;
use std::path::Path;

const MARKER: &str = "<!-- shark:related -->";

/// Returns `(repo, url)` for every PR created or found. `skip` names repos whose work already
/// landed: after a squash merge their commits still count as ahead of the base, and pushing
/// them again would open a duplicate PR.
pub fn create_linked_prs(
    ws: &Workspace,
    dir: &Path,
    skip: &[String],
) -> Result<Vec<(String, String)>, String> {
    let dirty: Vec<&str> = ws
        .repos
        .iter()
        .filter(|r| {
            !git(&dir.join(&r.name), &["status", "--porcelain"])
                .unwrap_or_default()
                .is_empty()
        })
        .map(|r| r.name.as_str())
        .collect();
    if !dirty.is_empty() {
        return Err(format!(
            "uncommitted changes in: {} (ask the agent to commit first)",
            dirty.join(", ")
        ));
    }
    let mut prs = vec![];
    for r in &ws.repos {
        let wt = dir.join(&r.name);
        let ahead = git(&wt, &["rev-list", "--count", &format!("{}..HEAD", r.base)])?;
        if ahead == "0" || skip.contains(&r.name) {
            continue;
        }
        cmd_bounded(&wt, "git", &["push", "-u", "origin", &ws.branch], 120)?;
        // Only an open PR from this repo is reused; `gh pr view <branch>` would also return a
        // merged one, and new commits after a merge need a new PR.
        let open = cmd_bounded(
            &wt,
            "gh",
            &[
                "pr",
                "list",
                "--head",
                &ws.branch,
                "--state",
                "open",
                "--json",
                "url,isCrossRepository",
                "-q",
                "[.[] | select(.isCrossRepository | not)][0].url // empty",
            ],
            30,
        )?;
        let url = match open.is_empty() {
            false => open,
            true => {
                let base = r.base.strip_prefix("origin/").unwrap_or(&r.base);
                let out = cmd_bounded(
                    &wt,
                    "gh",
                    &[
                        "pr", "create", "--head", &ws.branch, "--base", base, "--fill",
                    ],
                    60,
                )?;
                out.lines().last().unwrap_or_default().to_string()
            }
        };
        prs.push((r.name.clone(), url));
    }
    if prs.is_empty() {
        return Err("no repo has commits ahead of its base".into());
    }
    if prs.len() > 1 {
        for (r, url) in &prs {
            let wt = dir.join(r);
            let body = cmd_bounded(
                &wt,
                "gh",
                &["pr", "view", url, "--json", "body", "-q", ".body"],
                30,
            )?;
            cmd_bounded(
                &wt,
                "gh",
                &["pr", "edit", url, "--body", &link_body(&body, &prs, url)],
                30,
            )?;
        }
    }
    Ok(prs)
}

/// Replaces everything after `MARKER` with links to the other PRs, so re-runs are idempotent.
pub fn link_body(body: &str, prs: &[(String, String)], own: &str) -> String {
    let mut out = body.split(MARKER).next().unwrap().trim_end().to_string();
    out += &format!("\n\n{MARKER}\n## Related PRs\n");
    for (repo, url) in prs.iter().filter(|(_, u)| u != own) {
        out += &format!("- {repo}: {url}\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_body_is_idempotent() {
        let prs = vec![
            ("api".to_string(), "https://x/api/1".to_string()),
            ("web".into(), "https://x/web/2".into()),
        ];
        let once = link_body("Adds login.", &prs, "https://x/api/1");
        assert!(once.starts_with("Adds login.\n\n"));
        assert!(once.contains("- web: https://x/web/2") && !once.contains("api/1"));
        assert_eq!(link_body(&once, &prs, "https://x/api/1"), once);
    }
}
