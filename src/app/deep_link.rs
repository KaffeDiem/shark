//! `shark://open?url=<GitHub PR URL>`: the browser extension's way in. Opens the task that
//! already has the PR, otherwise starts one on it, the way the branch/PR picker would.
use super::*;
use crate::pr::Target;

/// The PR a `shark://open?url=…` link names.
pub(crate) fn parse_link(link: &str) -> Result<Target, String> {
    let url = url::Url::parse(link).map_err(|e| format!("Bad link: {e}"))?;
    if url.scheme() != "shark" || url.host_str() != Some("open") {
        return Err(format!("Unknown link: {link}"));
    }
    let pr = (url.query_pairs().find(|(k, _)| k == "url"))
        .ok_or("The link names no pull request")?
        .1;
    Target::from_url(&pr)
}

impl Shark {
    /// Links come from outside, so every failure is a toast, never a dialog.
    pub(crate) fn open_link(&mut self, link: &str, window: &mut Window, cx: &mut Context<Self>) {
        let target = match parse_link(link) {
            Ok(t) => t,
            Err(e) => return notify_err(window, cx, e),
        };
        let Some(repo) = (self.state.repos.iter())
            .find(|r| git::origin_is(&r.path, &target))
            .cloned()
        else {
            let (o, r) = (&target.owner, &target.repo);
            return notify_err(window, cx, format!("Add {o}/{r} as a repository first"));
        };
        // Known without GitHub: review checkouts remember their PR, and polled tasks hold theirs.
        if let Some(name) = self.task_with_pr(&repo, &target, None, cx) {
            return self.open_task_named(&name, window, cx);
        }
        if self.state.read_only {
            return notify_err(
                window,
                cx,
                "Can't create tasks: state.json couldn't be read",
            );
        }
        // A second click while the first is still checking out.
        if self.creating.contains_key(&format!("pr-{}", target.number)) {
            return;
        }
        window.push_notification(format!("Looking up #{}…", target.number), cx);
        let (src, url) = (repo.path.clone(), target.url());
        cx.spawn_in(window, async move |this, cx| {
            let view = cx
                .background_executor()
                .spawn(async move {
                    let fields = "number,title,url,state,headRefName,baseRefName,isCrossRepository";
                    let args = ["pr", "view", &url, "--json", fields];
                    let json = git::cmd_bounded(&src, "gh", &args, 20)?;
                    serde_json::from_str::<crate::prs::Pr>(&json).map_err(|e| format!("gh: {e}"))
                })
                .await;
            this.update_in(cx, |s, window, cx| match view {
                Err(e) => notify_err(window, cx, e),
                Ok(v) => {
                    // The PR's own branch may already be a task's; a fork's branch name is not.
                    let head = (!v.is_cross_repository).then_some(v.head_ref_name.as_str());
                    if let Some(name) = s.task_with_pr(&repo, &target, head, cx) {
                        return s.open_task_named(&name, window, cx);
                    }
                    let branch = format!("pr/{}", target.number);
                    let pr = git::PullRequest {
                        number: v.number,
                        url: v.url,
                        base: v.base_ref_name,
                    };
                    s.create_task_on(
                        branch.clone(),
                        vec![(repo.clone(), branch)],
                        vec![(repo.name, pr)],
                        vec![],
                        None,
                        None,
                        v.title,
                        None,
                        false,
                        window,
                        cx,
                    )
                }
            })
            .ok();
        })
        .detach();
    }

    /// The task holding `target` in `repo`: a review checkout of it, a task whose polled PR
    /// is it, or (with `head`) one working on the PR's own branch.
    fn task_with_pr(
        &self,
        repo: &Repo,
        target: &Target,
        head: Option<&str>,
        cx: &App,
    ) -> Option<String> {
        let url = target.url().to_ascii_lowercase();
        let same = |u: &str| u.trim_end_matches('/').eq_ignore_ascii_case(&url);
        self.state.tasks.iter().find_map(|t| {
            let r = t.repos.iter().find(|r| r.src == repo.path)?;
            let branch = t.branch_of(r);
            let found = head == Some(branch)
                || git::review_pr_url(&repo.path, branch).is_some_and(|u| same(&u))
                || (self.syncs.get(&t.name)).is_some_and(|s| {
                    (s.read(cx).pr_urls().iter()).any(|(n, u)| *n == r.name && same(u))
                });
            found.then(|| t.name.clone())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro

    #[test]
    fn links_name_a_pull_request() {
        let t = parse_link("shark://open?url=https%3A%2F%2Fgithub.com%2Facme%2Fapi%2Fpull%2F7")
            .unwrap();
        assert_eq!(
            (t.host.as_str(), t.owner.as_str(), t.repo.as_str(), t.number),
            ("github.com", "acme", "api", 7)
        );
        assert_eq!(
            parse_link("shark://open?url=https://github.com/acme/api/pull/7").unwrap(),
            t
        );
        for bad in [
            "shark://open",
            "shark://open?url=https://github.com/acme/api",
            "shark://pr?url=https://github.com/acme/api/pull/7",
            "https://github.com/acme/api/pull/7",
            "not a url",
        ] {
            assert!(parse_link(bad).is_err(), "{bad}");
        }
    }
}
