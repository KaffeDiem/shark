//! Recent and open PRs of a workspace's repos as `gh pr list` gives them, with an on-disk cache
//! so the `#` completion and the New workspace dialog are instant after a restart.
use crate::state::support_dir;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PrStatus {
    Open,
    Draft,
    Merged,
    Closed,
}

impl PrStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Draft => "draft",
            Self::Merged => "merged",
            Self::Closed => "closed",
        }
    }

    pub fn icon(self, theme: &gpui_kit::component::Theme) -> gpui_kit::component::Icon {
        use gpui_kit::{Styled as _, assets::IconName, component::Icon};
        let (icon, color) = match self {
            Self::Open => (IconName::GitPullRequest, theme.success_foreground),
            Self::Draft => (IconName::GitPullRequestDraft, theme.muted_foreground),
            Self::Merged => (IconName::GitMerge, theme.muted_foreground),
            Self::Closed => (IconName::GitPullRequestClosed, theme.danger_foreground),
        };
        Icon::new(icon).text_color(color)
    }
}

/// A PR as `gh pr list --json` gives it, and as cached on disk.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Pr {
    /// The workspace repo it was listed in; not from gh.
    #[serde(default)]
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub author: Option<Author>,
    #[serde(default)]
    pub head_ref_name: String,
    #[serde(default)]
    pub base_ref_name: String,
    pub url: String,
    /// `OPEN`, `MERGED` or `CLOSED`.
    pub state: String,
    #[serde(default)]
    pub is_draft: bool,
    #[serde(default)]
    pub updated_at: String,
    /// From a fork: its head branch isn't in this repo, so nothing can stack on it.
    #[serde(default)]
    pub is_cross_repository: bool,
}

impl Pr {
    pub fn status(&self) -> PrStatus {
        match (self.state.as_str(), self.is_draft) {
            ("MERGED", _) => PrStatus::Merged,
            ("CLOSED", _) => PrStatus::Closed,
            (_, true) => PrStatus::Draft,
            _ => PrStatus::Open,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Author {
    pub login: String,
}

const PR_ARGS: [&str; 10] = [
    "pr",
    "list",
    "--state",
    "all",
    "--limit",
    "50",
    "--search",
    "sort:updated-desc",
    "--json",
    "number,title,author,headRefName,baseRefName,url,state,isDraft,updatedAt,isCrossRepository",
];

/// Recent PRs for chat, or open PRs for checkout; successful fetches replace this repo's cache.
pub fn fetch_repo_prs(repo: &str, src: &Path, open: bool) -> Result<Vec<Pr>, String> {
    let mut args = PR_ARGS;
    if open {
        args[3] = "open";
        // shortcut: at most 1000 open PRs, paginate when repositories exceed this limit.
        args[5] = "1000";
    }
    let json = crate::git::cmd_bounded(src, "gh", &args, 15).or_else(|e| match e {
        _ if e.contains("none of the git remotes") => Ok("[]".into()),
        _ => Err(pr_error(&e)),
    })?;
    let prs = parse_prs(repo, &json)?;
    save_repo_prs(src, open, &prs);
    Ok(prs)
}

pub(crate) fn parse_prs(repo: &str, json: &str) -> Result<Vec<Pr>, String> {
    let mut prs: Vec<Pr> = serde_json::from_str(json).map_err(|e| format!("gh: {e}"))?;
    for p in &mut prs {
        p.repo = repo.to_string();
    }
    Ok(prs)
}

/// `cmd_bounded`'s error, minus the command line it starts with.
fn pr_error(e: &str) -> String {
    let stderr = e
        .strip_prefix("gh ")
        .and_then(|s| s.split_once(": ").map(|(_, stderr)| stderr))
        .unwrap_or(e);
    match stderr {
        _ if e.starts_with("gh: ") || e == "gh not found on PATH" => "gh isn't installed".into(),
        _ if stderr.contains("gh auth login") => "not logged in: run gh auth login".into(),
        _ if stderr.starts_with("timed out") => "GitHub didn't answer in 15s".into(),
        _ => stderr.lines().last().unwrap_or("gh failed").to_string(),
    }
}

fn prs_path(src: &Path, open: bool) -> PathBuf {
    let key = crate::files::fingerprint(src.as_os_str().as_encoded_bytes());
    let scope = if open { "open" } else { "recent" };
    support_dir()
        .join("prs")
        .join(format!("{key:016x}-{scope}.json"))
}

/// Source paths are shared by the creation dialog and every workspace using that repo.
pub fn cached_repo_prs(repo: &str, src: &Path, open: bool) -> Vec<Pr> {
    std::fs::read_to_string(prs_path(src, open))
        .ok()
        .and_then(|json| parse_prs(repo, &json).ok())
        .unwrap_or_default()
}

fn save_repo_prs(src: &Path, open: bool, prs: &[Pr]) {
    if let Ok(json) = serde_json::to_vec(prs) {
        let _ = crate::state::write(&prs_path(src, open), &json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use crate::complete::{Kind, pr_items};

    #[test]
    fn pr_status_survives_cache_and_completion_and_terminal_state_wins() {
        for (state, draft, expected) in [
            ("OPEN", false, PrStatus::Open),
            ("OPEN", true, PrStatus::Draft),
            ("MERGED", false, PrStatus::Merged),
            ("MERGED", true, PrStatus::Merged),
            ("CLOSED", false, PrStatus::Closed),
            ("CLOSED", true, PrStatus::Closed),
        ] {
            let json = format!(
                r#"[{{"number":7,"title":"Review","author":null,"url":"https://github.com/o/api/pull/7","state":"{state}","isDraft":{draft}}}]"#
            );
            let fetched = parse_prs("api", &json).unwrap();
            assert_eq!(fetched[0].status(), expected);
            let cached = parse_prs("api", &serde_json::to_string(&fetched).unwrap()).unwrap();
            let items = pr_items(&cached);
            assert_eq!(items[0].kind, Kind::Pr(expected));
            if expected != PrStatus::Open {
                assert!(items[0].detail.ends_with(expected.label()));
            }
        }
    }

    #[test]
    fn repo_pr_cache_separates_scopes_and_survives_failed_refresh() {
        let src =
            std::env::temp_dir().join(format!("shark-missing-pr-source-{}", std::process::id()));
        assert!(!src.exists());
        let recent = parse_prs("api", r#"[{"number":1,"title":"Merged","author":null,"url":"https://github.com/o/api/pull/1","state":"MERGED"}]"#).unwrap();
        let open = parse_prs("api", r#"[{"number":2,"title":"Review","author":{"login":"kim"},"headRefName":"feature","baseRefName":"main","url":"https://github.com/o/api/pull/2","state":"OPEN","isDraft":true}]"#).unwrap();
        save_repo_prs(&src, false, &recent);
        save_repo_prs(&src, true, &open);
        assert_eq!(cached_repo_prs("renamed", &src, false)[0].number, 1);
        let cached = cached_repo_prs("renamed", &src, true);
        assert_eq!(cached[0].number, 2);
        assert_eq!(cached[0].repo, "renamed");
        assert_eq!(cached[0].base_ref_name, "main");
        assert!(cached[0].is_draft);
        assert!(cached_repo_prs("api", &src.join("other"), true).is_empty());
        assert!(fetch_repo_prs("api", &src, true).is_err());
        assert_eq!(cached_repo_prs("api", &src, true)[0].number, 2);
        fs::write(prs_path(&src, true), "not JSON").unwrap();
        assert!(cached_repo_prs("api", &src, true).is_empty());
        fs::remove_file(prs_path(&src, false)).unwrap();
        fs::remove_file(prs_path(&src, true)).unwrap();
    }

    #[test]
    fn gh_failures_read_briefly() {
        assert!(parse_prs("api", "").is_err());
        assert_eq!(
            pr_error("gh: No such file or directory (os error 2)"),
            "gh isn't installed"
        );
        let e = format!(
            "gh {}: To get started with GitHub CLI, please run:  gh auth login",
            PR_ARGS.join(" ")
        );
        assert_eq!(pr_error(&e), "not logged in: run gh auth login");
    }
}
