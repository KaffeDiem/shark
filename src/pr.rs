//! Coordinated PRs: one per repo with commits, each body linking all the others.
use crate::git::{cmd_bounded, dirty, git};
use crate::state::Workspace;
use std::path::Path;

pub mod git_feature;
mod review;

pub use review::{Anchor, publish_note, publish_review, verify_original_anchor};

const MARKER: &str = "<!-- shark:related -->";

/// How new PRs are opened. The composer's first line is the title and the rest the body;
/// empty falls back to `--fill` (commit info). `--fill` stays on, so a title alone still gets
/// the commits as body: gh lets `--title`/`--body` override what it fills in.
pub struct NewPr {
    pub title: String,
    pub body: String,
    pub draft: bool,
}

impl NewPr {
    pub fn new(composer: &str, draft: bool) -> NewPr {
        let (title, body) = composer
            .trim()
            .split_once('\n')
            .unwrap_or((composer.trim(), ""));
        NewPr {
            title: title.trim().into(),
            body: body.trim().into(),
            draft,
        }
    }

    fn args<'a>(&'a self, branch: &'a str, base: &'a str) -> Vec<&'a str> {
        let mut a = vec!["pr", "create", "--head", branch, "--base", base, "--fill"];
        for (flag, v) in [("--title", &self.title), ("--body", &self.body)] {
            if !v.is_empty() {
                a.extend([flag, v]);
            }
        }
        if self.draft {
            a.push("--draft");
        }
        a
    }
}

/// `(repo, url)` per PR, and the warning when linking failed.
pub type Created = (Vec<(String, String)>, Option<String>);

/// Returns `(repo, url)` for every PR created or found, and a warning when they exist but
/// linking their bodies failed. `skip` names repos whose work already
/// landed: after a squash merge their commits still count as ahead of the base, and pushing
/// them again would open a duplicate PR.
pub fn create_linked_prs(
    ws: &Workspace,
    dir: &Path,
    skip: &[String],
    new: &NewPr,
) -> Result<Created, String> {
    for r in &ws.repos {
        if crate::git::review_pr_url(&dir.join(&r.name), ws.branch_of(r)).is_some() {
            return Err(format!(
                "{} is a local PR review checkout; create a regular workspace to publish changes",
                r.name
            ));
        }
    }
    let mut dirty_repos = vec![];
    for r in &ws.repos {
        if dirty(&dir.join(&r.name)).map_err(|e| format!("{}: {e}", r.name))? {
            dirty_repos.push(r.name.as_str());
        }
    }
    if !dirty_repos.is_empty() {
        return Err(format!(
            "uncommitted changes in: {} (ask the agent to commit first)",
            dirty_repos.join(", ")
        ));
    }
    // Validate every participating repo before pushing or creating any PR.
    let mut pending = vec![];
    for r in ws.repos.iter().filter(|r| !skip.contains(&r.name)) {
        let wt = dir.join(&r.name);
        let branch = ws.branch_of(r);
        if git(&wt, &["symbolic-ref", "--quiet", "HEAD"]) != Ok(format!("refs/heads/{branch}")) {
            return Err(format!(
                "{}: switch to branch `{branch}` before creating a PR",
                r.name
            ));
        }
        // A stacked branch's PR merges into its parent, which must be on GitHub.
        if !crate::git::has_ref(&wt, &r.base) {
            let b = crate::sync::short(&r.base);
            return Err(format!(
                "{}: its base `{b}` isn't on GitHub; push it first (from its workspace), or Change base…",
                r.name
            ));
        }
        let ahead = git(&wt, &["rev-list", "--count", &format!("{}..HEAD", r.base)])?;
        if ahead != "0" {
            pending.push(r);
        }
    }
    if pending.is_empty() {
        return Err("no repo has commits ahead of its base".into());
    }
    let mut prs = vec![];
    for r in pending {
        let wt = dir.join(&r.name);
        let branch = ws.branch_of(r);
        cmd_bounded(
            &wt,
            "git",
            &["push", "-u", "origin", &format!("refs/heads/{branch}")],
            120,
        )?;
        // Same `--repo` as polling, so a fork's upstream never answers for origin.
        let origin = crate::sync::origin_repo(&wt);
        let repo_flag = |a| with_repo(a, origin.as_deref());
        // Only an open PR from this repo is reused; `gh pr view <branch>` would also return a
        // merged one, and new commits after a merge need a new PR.
        let open = cmd_bounded(
            &wt,
            "gh",
            &repo_flag(vec![
                "pr",
                "list",
                "--head",
                branch,
                "--state",
                "open",
                "--json",
                "url,isCrossRepository",
                "-q",
                "[.[] | select(.isCrossRepository | not)][0].url // empty",
            ]),
            30,
        )?;
        let url = match open.is_empty() {
            false => open,
            true => {
                let base = crate::sync::short(&r.base);
                let out = cmd_bounded(&wt, "gh", &repo_flag(new.args(branch, base)), 60)?;
                last_line(&out)
            }
        };
        prs.push((r.name.clone(), url));
    }
    // The PRs exist by now: a failed link is a warning, not a lost result.
    let warning = link_prs(dir, &prs);
    Ok((prs, warning))
}

/// Rewrites each PR's body to link the others; the warning lists the repos that failed.
fn link_prs(dir: &Path, prs: &[(String, String)]) -> Option<String> {
    let mut failed = vec![];
    if prs.len() > 1 {
        for (r, url) in prs {
            let wt = dir.join(r);
            let link = || {
                let body = cmd_bounded(
                    &wt,
                    "gh",
                    &["pr", "view", url, "--json", "body", "-q", ".body"],
                    30,
                )?;
                cmd_bounded(
                    &wt,
                    "gh",
                    &["pr", "edit", url, "--body", &link_body(&body, prs, url)],
                    30,
                )
            };
            if let Err(e) = link() {
                failed.push(format!("{r}: {e}"));
            }
        }
    }
    (!failed.is_empty()).then(|| {
        format!(
            "The PRs were created, but linking them failed; rerun to retry:\n{}",
            failed.join("\n")
        )
    })
}

/// The last line of command output (gh prints the new PR's URL last).
fn last_line(s: &str) -> String {
    s.lines().last().unwrap_or_default().to_string()
}

/// `args` plus `--repo <origin>`, as polling passes it.
pub(crate) fn with_repo<'a>(mut args: Vec<&'a str>, origin: Option<&'a str>) -> Vec<&'a str> {
    args.extend(origin.into_iter().flat_map(|o| ["--repo", o]));
    args
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

pub(crate) use crate::proc::gh;

/// Runs gh to post something; an error is gh's last word, not the command line (which carries
/// the whole body).
pub(crate) fn gh_post(wt: &Path, args: &[&str], secs: u64) -> Result<String, String> {
    let gh = gh();
    cmd_bounded(wt, &gh, args, secs)
        .map_err(|e| gh_error(&e, &format!("{gh} {}: ", args.join(" "))))
}

/// `e` without `cmd`, its last line; a timeout may still have posted.
fn gh_error(e: &str, cmd: &str) -> String {
    let why = e.strip_prefix(cmd).unwrap_or(e);
    let why = why
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(why)
        .trim();
    match why.starts_with("timed out") {
        true => format!("{why}; it may have posted, so check the PR before retrying"),
        false => why.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gh_errors_are_brief() {
        let cmd = "gh pr comment 7 --body **a** · x\n\nlong: body: ";
        let e = format!("{cmd}Warning: x\nHTTP 502: Bad Gateway (https://api.github.com)\n");
        assert_eq!(
            gh_error(&e, cmd),
            "HTTP 502: Bad Gateway (https://api.github.com)"
        );
        let e = format!("{cmd}timed out after 60s");
        assert!(gh_error(&e, cmd).starts_with("timed out after 60s; it may have posted"));
        assert_eq!(
            gh_error("gh not found on PATH", cmd),
            "gh not found on PATH"
        );
    }

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

    #[test]
    fn linking_reports_failed_repos_and_skips_a_single_pr() {
        let none = std::env::temp_dir().join("shark-no-such-dir");
        let pr = |r: &str| (r.to_string(), format!("https://x/{r}/1"));
        assert_eq!(link_prs(&none, &[pr("api")]), None);
        let warning = link_prs(&none, &[pr("api"), pr("web")]).expect("both fail");
        assert!(
            warning.contains("api:") && warning.contains("web:"),
            "{warning}"
        );
        assert_eq!(last_line("Creating\nhttps://x/api/1\n"), "https://x/api/1");
        assert_eq!(last_line(""), "");
    }

    #[test]
    fn composer_text_titles_new_prs() {
        let fill = ["pr", "create", "--head", "b", "--base", "main", "--fill"];
        assert_eq!(NewPr::new("  ", false).args("b", "main"), fill);
        let p = NewPr::new("Add login\n\nUses OAuth.\n", true);
        let a = p.args("b", "main");
        assert_eq!(
            a[fill.len()..],
            ["--title", "Add login", "--body", "Uses OAuth.", "--draft"]
        );
        assert_eq!(
            NewPr::new("Just a title", false).args("b", "main").len(),
            fill.len() + 2
        );
    }

    /// Creation asks gh about origin like polling does (`--repo`), and a failed link step still
    /// returns the PRs it created, with a warning.
    #[test]
    fn creation_targets_origin_and_keeps_prs_when_linking_fails() {
        use crate::git::{add_repo, create_workspace};
        use std::fs;
        #[cfg(feature = "test-support")]
        let _lock = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        struct Restore(std::ffi::OsString);
        impl Drop for Restore {
            fn drop(&mut self) {
                unsafe { std::env::set_var("PATH", &self.0) };
            }
        }
        let _restore = Restore(std::env::var_os("PATH").unwrap());
        let root = std::env::temp_dir().join(format!("shark-pr-repo-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let c = |dir: &Path, args: &[&str]| {
            git(
                dir,
                &[&["-c", "user.name=t", "-c", "user.email=t@t"], args].concat(),
            )
            .unwrap()
        };
        let mut repos = vec![];
        for name in ["api", "web"] {
            let (origin, src) = (
                root.join(format!("{name}.git")),
                root.join("src").join(name),
            );
            fs::create_dir_all(&src).unwrap();
            git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]).unwrap();
            git(&src, &["init", "-q", "-b", "main"]).unwrap();
            fs::write(src.join("README.md"), "hi\n").unwrap();
            c(&src, &["add", "."]);
            c(&src, &["commit", "-qm", "init"]);
            // Origin reads as a GitHub remote; git still pushes to the bare repo.
            let url = format!("git@github.com:o/{name}.git");
            git(&src, &["remote", "add", "origin", &url]).unwrap();
            let rewrite = format!("url.{}.insteadOf", origin.display());
            git(&src, &["config", &rewrite, &url]).unwrap();
            git(&src, &["push", "-q", "origin", "main"]).unwrap();
            repos.push(add_repo(&src, &[]).unwrap());
        }
        let dir = root.join("ws/feat");
        let ws =
            create_workspace(&dir, "feat", "feat", &crate::git::on_branch(&repos, "feat")).unwrap();
        for name in ["api", "web"] {
            c(
                &dir.join(name),
                &["commit", "-q", "--allow-empty", "-m", "work"],
            );
        }
        let (bin, log) = (root.join("bin"), root.join("gh.log"));
        fs::create_dir_all(&bin).unwrap();
        let gh = bin.join("gh");
        fs::write(
            &gh,
            format!(
                r#"#!/bin/sh
# PATH is process-wide: other tests running now get the real gh.
case "$PWD" in */{tag}/*) ;; *) PATH='{orig}' exec gh "$@" ;; esac
echo "$(basename "$PWD") $*" >> {log}
case "$1 $2" in
"pr list") ;;
"pr create") echo https://github.com/o/$(basename "$PWD")/pull/1 ;;
"pr view") echo Body ;;
"pr edit") echo 'gh: edit denied' >&2; exit 1 ;;
*) exit 1 ;;
esac
"#,
                log = log.display(),
                orig = std::env::var("PATH").unwrap(),
                tag = root.file_name().unwrap().to_string_lossy(),
            ),
        )
        .unwrap();
        std::process::Command::new("chmod")
            .arg("+x")
            .arg(&gh)
            .status()
            .unwrap();
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
        // SAFETY: only prepends a dir holding `gh`; std serializes env access with spawns.
        unsafe { std::env::set_var("PATH", path) };

        let (prs, warning) = create_linked_prs(&ws, &dir, &[], &NewPr::new("", false)).unwrap();
        assert_eq!(
            prs,
            [
                (
                    "api".to_string(),
                    "https://github.com/o/api/pull/1".to_string()
                ),
                ("web".into(), "https://github.com/o/web/pull/1".into())
            ]
        );
        let warning = warning.expect("linking failed");
        assert!(
            warning.contains("created") && warning.contains("api:") && warning.contains("web:"),
            "{warning}"
        );
        assert!(warning.contains("edit denied"), "{warning}");
        let calls = fs::read_to_string(&log).unwrap();
        for repo in ["api", "web"] {
            let flag = format!("--repo https://github.com/o/{repo}");
            for verb in ["pr list", "pr create"] {
                assert!(
                    calls
                        .lines()
                        .any(|l| l.starts_with(&format!("{repo} {verb}")) && l.contains(&flag)),
                    "{verb} in {repo} lacks {flag}:\n{calls}"
                );
            }
        }
        fs::remove_dir_all(&root).unwrap();
    }

    /// Real git against bare origins, a fake `gh` on PATH that logs its argv and runs `-q`
    /// through jq (gh's own filter language) on canned `pr list` JSON.
    #[test]
    fn creates_and_links_prs() {
        use crate::git::{add_repo, create_workspace};
        use std::fs;
        #[cfg(feature = "test-support")]
        let _lock = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        struct Restore(std::ffi::OsString);
        impl Drop for Restore {
            fn drop(&mut self) {
                unsafe { std::env::set_var("PATH", &self.0) };
            }
        }
        let _restore = Restore(std::env::var_os("PATH").unwrap());
        let root = std::env::temp_dir().join(format!("shark-pr-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let c = |dir: &Path, args: &[&str]| {
            git(
                dir,
                &[&["-c", "user.name=t", "-c", "user.email=t@t"], args].concat(),
            )
            .unwrap()
        };
        let mut repos = vec![];
        // api: new PR. web: reuses its own open PR, not the fork's. docs: nothing ahead. ops: landed.
        for name in ["api", "web", "docs", "ops"] {
            let (origin, src) = (
                root.join(format!("{name}.git")),
                root.join("src").join(name),
            );
            fs::create_dir_all(&src).unwrap();
            git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]).unwrap();
            git(&src, &["init", "-q", "-b", "main"]).unwrap();
            fs::write(src.join("README.md"), "hi\n").unwrap();
            c(&src, &["add", "."]);
            c(&src, &["commit", "-qm", "init"]);
            git(
                &src,
                &["remote", "add", "origin", &origin.to_string_lossy()],
            )
            .unwrap();
            git(&src, &["push", "-q", "origin", "main"]).unwrap();
            repos.push(add_repo(&src, &[]).unwrap());
        }
        let dir = root.join("ws/feat");
        // api is on its own branch.
        let mut on = crate::git::on_branch(&repos, "feat");
        on[0].1 = "kd/api".into();
        let ws = create_workspace(&dir, "feat", "feat", &on).unwrap();
        for name in ["api", "web", "ops"] {
            c(
                &dir.join(name),
                &["commit", "-q", "--allow-empty", "-m", "work"],
            );
        }

        let (bin, log, fail) = (root.join("bin"), root.join("gh.log"), root.join("fail"));
        fs::create_dir_all(&bin).unwrap();
        let gh = bin.join("gh");
        fs::write(
            &gh,
            format!(
                r#"#!/bin/sh
# PATH is process-wide: other tests running now get the real gh.
case "$PWD" in */{tag}/*) ;; *) PATH='{orig}' exec gh "$@" ;; esac
repo=$(basename "$PWD")
case "$2" in create) echo "$repo $*" ;; *) echo "$repo $1 $2" ;; esac >> {log}
case "$1 $2" in
"pr list")
  while [ "$1" != -q ]; do shift; done
  if [ $repo = web ]; then
    echo '[{{"url":"https://gh/fork/9","isCrossRepository":true}},{{"url":"https://gh/web/7","isCrossRepository":false}}]'
  else echo '[]'; fi | jq -r "$2" ;;
"pr create") [ -e {fail} ] && {{ echo 'gh: boom' >&2; exit 1; }}; echo Creating; echo https://gh/$repo/1 ;;
"pr view") echo Body ;;
"pr edit") echo "$5" > {root}/$repo.body ;;
*) exit 1 ;;
esac
"#,
                log = log.display(),
                fail = fail.display(),
                root = root.display(),
                orig = std::env::var("PATH").unwrap(),
                tag = root.file_name().unwrap().to_string_lossy(),
            ),
        )
        .unwrap();
        std::process::Command::new("chmod")
            .args(["+x"])
            .arg(&gh)
            .status()
            .unwrap();
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
        // SAFETY: only prepends a dir holding `gh`; std serializes env access with spawns.
        unsafe { std::env::set_var("PATH", path) };

        fs::write(dir.join("docs/stray.txt"), "").unwrap();
        let err = create_linked_prs(&ws, &dir, &[], &NewPr::new("", false)).unwrap_err();
        assert!(err.contains("docs"), "{err}");
        fs::remove_file(dir.join("docs/stray.txt")).unwrap();

        git(
            &dir.join("web"),
            &[
                "config",
                "branch.feat.shark-pr-url",
                "https://github.com/o/web/pull/7",
            ],
        )
        .unwrap();
        let err = create_linked_prs(&ws, &dir, &[], &NewPr::new("", false)).unwrap_err();
        assert!(err.contains("local PR review checkout"), "{err}");
        assert!(!log.exists(), "refuse before any PR is created or linked");
        assert!(
            git(&root.join("api.git"), &["branch", "--list", "kd/api"])
                .unwrap()
                .is_empty()
        );
        git(
            &dir.join("web"),
            &["config", "--unset", "branch.feat.shark-pr-url"],
        )
        .unwrap();

        // An explicit PR action must not inspect HEAD but publish a different branch.
        git(&dir.join("web"), &["checkout", "-qb", "other"]).unwrap();
        let err = create_linked_prs(&ws, &dir, &[], &NewPr::new("", false)).unwrap_err();
        assert!(err.contains("web: switch to branch `feat`"), "{err}");
        assert!(
            !log.exists(),
            "all branches are checked before GitHub writes"
        );
        assert!(
            git(&root.join("api.git"), &["branch", "--list", "kd/api"])
                .unwrap()
                .is_empty(),
            "all branches are checked before pushes"
        );
        git(&dir.join("web"), &["checkout", "-q", "feat"]).unwrap();

        // A tag with the same name changes Git's short display name, not the checked-out branch.
        git(&dir.join("web"), &["tag", "feat"]).unwrap();

        // A later repo's invalid base must not publish an earlier repo first.
        let mut invalid = ws.clone();
        invalid.repos[1].base = "origin/missing-parent".into();
        let err = create_linked_prs(&invalid, &dir, &[], &NewPr::new("", false)).unwrap_err();
        assert!(err.contains("web: its base `missing-parent`"), "{err}");
        assert!(!log.exists(), "all bases are checked before GitHub writes");
        assert!(
            git(&root.join("api.git"), &["branch", "--list", "kd/api"])
                .unwrap()
                .is_empty(),
            "all bases are checked before pushes"
        );

        // A failed status read is not a clean worktree, including for a skipped repo.
        fs::rename(dir.join("ops"), dir.join("ops-away")).unwrap();
        let err =
            create_linked_prs(&ws, &dir, &["ops".into()], &NewPr::new("", false)).unwrap_err();
        assert!(err.starts_with("ops:"), "{err}");
        assert!(!log.exists(), "status errors refuse before publishing");
        fs::rename(dir.join("ops-away"), dir.join("ops")).unwrap();

        // A landed repo no longer needs its parent ref to publish the remaining repos.
        let mut ws = ws;
        ws.repos.iter_mut().find(|r| r.name == "ops").unwrap().base =
            "origin/deleted-parent".into();
        let (prs, warning) =
            create_linked_prs(&ws, &dir, &["ops".into()], &NewPr::new("", false)).unwrap();
        assert_eq!(warning, None);
        assert_eq!(
            prs,
            [
                ("api".to_string(), "https://gh/api/1".to_string()),
                ("web".into(), "https://gh/web/7".into())
            ]
        );
        assert_eq!(
            fs::read_to_string(&log).unwrap(),
            "api pr list\napi pr create --head kd/api --base main --fill\nweb pr list\napi pr view\napi pr edit\nweb pr view\nweb pr edit\n"
        );
        let body = fs::read_to_string(root.join("api.body")).unwrap();
        assert!(
            body.starts_with("Body\n\n") && body.contains("- web: https://gh/web/7"),
            "{body}"
        );
        let pushed = |name: &str, b: &str| {
            git(&root.join(format!("{name}.git")), &["branch", "--list", b]).unwrap()
        };
        assert!(!pushed("api", "kd/api").is_empty() && pushed("ops", "feat").is_empty());

        // A selected single repo cannot touch another PR body or remote branch.
        let other_body = fs::read(root.join("web.body")).unwrap();
        let other_head = git(&root.join("web.git"), &["rev-parse", "feat"]).unwrap();
        let mut selected = ws.clone();
        selected.repos.retain(|r| r.name == "api");
        fs::write(dir.join("web/not-selected"), "dirty").unwrap();
        fs::write(&log, "").unwrap();
        let (urls, _) =
            create_linked_prs(&selected, &dir, &[], &NewPr::new("Scoped", true)).unwrap();
        assert_eq!(urls, [("api".into(), "https://gh/api/1".into())]);
        let calls = fs::read_to_string(&log).unwrap();
        assert!(!calls.contains("web") && !calls.contains("pr edit") && calls.contains("--draft"));
        assert_eq!(fs::read(root.join("web.body")).unwrap(), other_body);
        assert_eq!(
            git(&root.join("web.git"), &["rev-parse", "feat"]).unwrap(),
            other_head
        );
        fs::remove_file(dir.join("web/not-selected")).unwrap();

        // A failing create surfaces gh's error.
        fs::write(&fail, "").unwrap();
        let err =
            create_linked_prs(&ws, &dir, &["ops".into()], &NewPr::new("", false)).unwrap_err();
        assert!(err.contains("gh: boom"), "{err}");
        fs::remove_file(&fail).unwrap();

        // Stacked: the PR targets the parent, which must be on GitHub first.
        let mut stacked = ws.clone();
        stacked.repos.retain(|r| r.name == "api");
        stacked.repos[0].base = "origin/logic".into();
        let before = fs::read_to_string(&log).unwrap();
        let err = create_linked_prs(&stacked, &dir, &[], &NewPr::new("", false)).unwrap_err();
        assert!(
            err.contains("api: its base `logic` isn't on GitHub; push it first"),
            "{err}"
        );
        assert_eq!(fs::read_to_string(&log).unwrap(), before, "no gh call");
        let api = dir.join("api");
        git(
            &api,
            &["push", "-q", "origin", "origin/main:refs/heads/logic"],
        )
        .unwrap();
        git(&api, &["fetch", "-q", "origin"]).unwrap();
        create_linked_prs(&stacked, &dir, &[], &NewPr::new("", false)).unwrap();
        let log = fs::read_to_string(&log).unwrap();
        assert!(
            log.ends_with("api pr create --head kd/api --base logic --fill\n"),
            "{log}"
        );
        fs::remove_dir_all(&root).unwrap();
    }
}
