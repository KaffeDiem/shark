//! Coordinated PRs: one per repo with commits, each body linking all the others.
use crate::git::{cmd_bounded, dirty, git};
use crate::state::Workspace;
use std::path::Path;

pub mod git_feature;

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

/// Repo names in merge order: `saved` first (names no longer in the workspace dropped), then
/// the rest in the order they were added.
pub fn merge_order(ws: &Workspace, saved: &[String]) -> Vec<String> {
    let names: Vec<String> = ws.repos.iter().map(|r| r.name.clone()).collect();
    let mut out: Vec<String> = saved
        .iter()
        .filter(|n| names.contains(n))
        .cloned()
        .collect();
    out.extend(names.into_iter().filter(|n| !saved.contains(n)));
    out
}

/// Moves `name` one place earlier (`earlier`) or later in `order`; unchanged at the ends.
pub fn move_in_order(order: &mut [String], name: &str, earlier: bool) {
    if let Some(i) = order.iter().position(|n| n == name) {
        let j = if earlier { i.wrapping_sub(1) } else { i + 1 };
        if j < order.len() {
            order.swap(i, j);
        }
    }
}

/// Returns `(repo, url)` for every PR created or found. `skip` names repos whose work already
/// landed: after a squash merge their commits still count as ahead of the base, and pushing
/// them again would open a duplicate PR.
pub fn create_linked_prs(
    ws: &Workspace,
    dir: &Path,
    skip: &[String],
    new: &NewPr,
) -> Result<Vec<(String, String)>, String> {
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
        if git(&wt, &["rev-parse", "--verify", "--quiet", &r.base]).is_err() {
            let b = r.base.strip_prefix("origin/").unwrap_or(&r.base);
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
        // Only an open PR from this repo is reused; `gh pr view <branch>` would also return a
        // merged one, and new commits after a merge need a new PR.
        let open = cmd_bounded(
            &wt,
            "gh",
            &[
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
            ],
            30,
        )?;
        let url = match open.is_empty() {
            false => open,
            true => {
                let base = r.base.strip_prefix("origin/").unwrap_or(&r.base);
                let out = cmd_bounded(&wt, "gh", &new.args(branch, base), 60)?;
                out.lines().last().unwrap_or_default().to_string()
            }
        };
        prs.push((r.name.clone(), url));
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

/// gh, or `SHARK_GH` (tests point it at a fake, as `SHARK_CLAUDE` for claude).
pub(crate) fn gh() -> String {
    std::env::var("SHARK_GH").unwrap_or_else(|_| "gh".into())
}

/// Runs gh without exposing request bodies from its command line in errors.
fn gh_request(wt: &Path, args: &[&str], secs: u64) -> Result<String, String> {
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

/// Posts `body` to PR `number` of the repo at `wt` as a conversation comment (a reviewer
/// note); returns its URL.
pub fn publish_note(wt: &Path, number: u64, body: &str) -> Result<String, String> {
    let n = number.to_string();
    let out = gh_request(wt, &["pr", "comment", &n, "--body", body], 60)?;
    Ok(out.lines().last().unwrap_or_default().to_string())
}

/// Where an inline review comment sits in a PR: repo-relative path, last (and first) line,
/// and the side: the base's lines (`left`) or the head's.
pub struct Anchor {
    pub path: String,
    pub line: usize,
    pub start: Option<usize>,
    pub left: bool,
}

/// Verify against fresh GitHub data; any unavailable or changing evidence means excerpt-only.
pub fn verify_original_anchor(
    wt: &Path,
    number: u64,
    revision: &str,
    anchor: &Anchor,
) -> Option<(String, bool)> {
    let read = |endpoint: &str| -> Option<serde_json::Value> {
        serde_json::from_str(&cmd_bounded(wt, &gh(), &["api", endpoint], 30).ok()?).ok()
    };
    let endpoint = format!("repos/{{owner}}/{{repo}}/pulls/{number}");
    let commits = |v: &serde_json::Value| -> Option<(String, String)> {
        let sha = |v: &serde_json::Value| {
            v.as_str()
                .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                .map(String::from)
        };
        Some((sha(&v["base"]["sha"])?, sha(&v["head"]["sha"])?))
    };
    let first = commits(&read(&endpoint)?)?;
    let comparison = read(&format!(
        "repos/{{owner}}/{{repo}}/compare/{}...{}",
        first.0, first.1
    ));
    let valid = comparison
        .as_ref()
        .is_some_and(|comparison| original_anchor_matches(comparison, revision, anchor));
    let unchanged = read(&endpoint).and_then(|v| commits(&v)).as_ref() == Some(&first);
    Some((first.1, valid && unchanged))
}

fn original_anchor_matches(
    comparison: &serde_json::Value,
    revision: &str,
    anchor: &Anchor,
) -> bool {
    if revision.is_empty() || comparison["merge_base_commit"]["sha"].as_str() != Some(revision) {
        return false;
    }
    let Some(file) = comparison["files"].as_array().and_then(|files| {
        files
            .iter()
            .find(|f| f["filename"].as_str() == Some(&anchor.path))
    }) else {
        return false;
    };
    if !matches!(file["status"].as_str(), Some("modified" | "removed"))
        || !file["previous_filename"].is_null()
    {
        return false;
    }
    let Some(patch) = file["patch"].as_str() else {
        return false;
    };
    let valid = || -> Option<bool> {
        let range = |s: &str| -> Option<(usize, usize)> {
            let (start, count) = s.split_once(',').unwrap_or((s, "1"));
            Some((start.parse().ok()?, count.parse().ok()?))
        };
        let mut lines = patch.lines().peekable();
        let (mut found, mut added, mut removed) = (false, 0u64, 0u64);
        while let Some(line) = lines.next() {
            let mut parts = line.strip_prefix("@@ -")?.split(' ');
            let (old, old_count) = range(parts.next()?)?;
            let (_, new_count) = range(parts.next()?.strip_prefix('+')?)?;
            if parts.next()? != "@@" {
                return None;
            }
            let (mut olds, mut news) = (0, 0);
            while lines.peek().is_some_and(|l| !l.starts_with("@@ ")) {
                match lines.next()?.as_bytes().first() {
                    Some(b' ') => {
                        olds += 1;
                        news += 1;
                    }
                    Some(b'-') => {
                        olds += 1;
                        removed += 1;
                    }
                    Some(b'+') => {
                        news += 1;
                        added += 1;
                    }
                    Some(b'\\') => {}
                    _ => return None,
                }
            }
            if (olds, news) != (old_count, new_count) {
                return None;
            }
            let start = anchor.start.unwrap_or(anchor.line);
            found |= start > 0
                && start <= anchor.line
                && start >= old
                && anchor.line < old.checked_add(old_count)?;
        }
        // Counts also detect patches truncated at a hunk boundary.
        Some(
            found && file["additions"].as_u64()? == added && file["deletions"].as_u64()? == removed,
        )
    };
    valid().unwrap_or(false)
}

/// Posts a Comment review (neither approves nor requests changes) to PR `number` at commit
/// `head`: `body` alone, or with `inline` as a comment on its lines (then `body` is the
/// review's note). Returns the review's URL.
pub fn publish_review(
    wt: &Path,
    number: u64,
    head: &str,
    body: &str,
    inline: Option<(&Anchor, &str)>,
) -> Result<String, String> {
    let args = review_api_args(&number.to_string(), head, body, inline);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    gh_request(wt, &args, 60)
}

/// `gh api` for a review, with one inline comment when given; gh fills in `{owner}/{repo}`.
fn review_api_args(
    n: &str,
    head: &str,
    body: &str,
    inline: Option<(&Anchor, &str)>,
) -> Vec<String> {
    let mut v: Vec<String> = ["api", "--method", "POST"].map(String::from).into();
    v.push(format!("repos/{{owner}}/{{repo}}/pulls/{n}/reviews"));
    let mut field = |flag: &str, kv: String| v.extend([flag.to_string(), kv]);
    field("-f", "event=COMMENT".into());
    field("-f", format!("commit_id={head}"));
    field("-f", format!("body={body}"));
    if let Some((a, text)) = inline {
        let side = if a.left { "LEFT" } else { "RIGHT" };
        field("-f", format!("comments[][path]={}", a.path));
        field("-F", format!("comments[][line]={}", a.line));
        field("-f", format!("comments[][side]={side}"));
        if let Some(s) = a.start {
            field("-F", format!("comments[][start_line]={s}"));
            field("-f", format!("comments[][start_side]={side}"));
        }
        field("-f", format!("comments[][body]={text}"));
    }
    v.extend(["-q".into(), ".html_url".into()]);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "test-support")]
    #[test]
    fn original_anchor_rechecks_moving_github_commits() {
        use serde_json::json;
        use std::{fs, os::unix::fs::PermissionsExt};
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("shark-anchor-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let script = root.join("gh");
        fs::write(
            &script,
            r#"#!/bin/sh
case "$2" in
*/compare/*) cat comparison.json; if [ -e move ]; then cp moved.json pr.json; fi ;;
*/pulls/*) cat pr.json ;;
*) exit 1 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let previous = std::env::var_os("SHARK_GH");
        struct Restore(Option<std::ffi::OsString>);
        impl Drop for Restore {
            fn drop(&mut self) {
                unsafe {
                    match &self.0 {
                        Some(v) => std::env::set_var("SHARK_GH", v),
                        None => std::env::remove_var("SHARK_GH"),
                    }
                }
            }
        }
        let _restore = Restore(previous);
        unsafe {
            std::env::set_var("SHARK_GH", &script);
        }
        let base = "a".repeat(40);
        let head = "b".repeat(40);
        fs::write(
            root.join("pr.json"),
            json!({"base":{"sha":base},"head":{"sha":head}}).to_string(),
        )
        .unwrap();
        fs::write(
            root.join("moved.json"),
            json!({"base":{"sha":base},"head":{"sha":"c".repeat(40)}}).to_string(),
        )
        .unwrap();
        fs::write(root.join("comparison.json"), json!({"merge_base_commit":{"sha":base},"files":[{"filename":"a.rs","status":"modified","patch":"@@ -1 +1 @@\n-old\n+new","additions":1,"deletions":1}]}).to_string()).unwrap();
        let anchor = Anchor {
            path: "a.rs".into(),
            line: 1,
            start: None,
            left: true,
        };
        assert_eq!(
            verify_original_anchor(&root, 1, &base, &anchor),
            Some((head.clone(), true))
        );
        assert_eq!(
            verify_original_anchor(&root, 1, "legacy", &anchor),
            Some((head.clone(), false))
        );
        fs::write(root.join("move"), "").unwrap();
        assert_eq!(
            verify_original_anchor(&root, 1, &base, &anchor),
            Some((head, false))
        );
        fs::remove_file(root.join("comparison.json")).unwrap();
        assert_eq!(
            verify_original_anchor(&root, 1, &base, &anchor),
            Some(("c".repeat(40), false))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn original_anchors_require_complete_matching_comparison() {
        use serde_json::json;
        let anchor = Anchor {
            path: "a.rs".into(),
            line: 3,
            start: Some(2),
            left: true,
        };
        let comparison = json!({"merge_base_commit":{"sha":"base"}, "files":[{
            "filename":"a.rs", "status":"modified", "patch":"@@ -2,2 +2,2 @@\n context\n-old\n+new", "additions":1, "deletions":1
        }]});
        assert!(original_anchor_matches(&comparison, "base", &anchor));
        for revision in ["", "HEAD", "different-base"] {
            assert!(!original_anchor_matches(&comparison, revision, &anchor));
        }
        for (key, value) in [
            ("status", json!("renamed")),
            ("patch", json!(null)),
            ("patch", json!("@@ -2,2 +2,2 @@\n context")),
            ("additions", json!(2)),
        ] {
            let mut changed = comparison.clone();
            changed["files"][0][key] = value;
            assert!(!original_anchor_matches(&changed, "base", &anchor));
        }
        let outside = Anchor { line: 4, ..anchor };
        assert!(!original_anchor_matches(&comparison, "base", &outside));
    }

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
    fn inline_review_args() {
        let a = Anchor {
            path: "src/app.rs".into(),
            line: 632,
            start: Some(631),
            left: false,
        };
        let args = review_api_args(
            "128",
            "abc",
            "app.rs:631-632",
            Some((&a, "Keep it aligned.")),
        );
        assert_eq!(
            args.join(" "),
            "api --method POST repos/{owner}/{repo}/pulls/128/reviews -f event=COMMENT \
             -f commit_id=abc -f body=app.rs:631-632 -f comments[][path]=src/app.rs \
             -F comments[][line]=632 -f comments[][side]=RIGHT \
             -F comments[][start_line]=631 -f comments[][start_side]=RIGHT \
             -f comments[][body]=Keep it aligned. -q .html_url"
        );
        let one = Anchor {
            start: None,
            left: true,
            ..a
        };
        let args = review_api_args("7", "h", "note", Some((&one, "x"))).join(" ");
        assert!(
            args.contains("-f body=note") && args.contains("side]=LEFT"),
            "{args}"
        );
        assert!(!args.contains("start_line"));
        // Without an inline comment: the review alone.
        let args = review_api_args("7", "h", "note", None).join(" ");
        assert!(args.ends_with("-f body=note -q .html_url"), "{args}");
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

    #[test]
    fn merge_order_follows_saved_then_added() {
        let ws = Workspace {
            name: "w".into(),
            branch: "b".into(),
            repos: ["api", "web", "ios"]
                .map(|n| crate::state::WsRepo {
                    name: n.into(),
                    src: n.into(),
                    base: "origin/main".into(),
                    branch: None,
                })
                .into(),
            ..Default::default()
        };
        let o = merge_order(&ws, &["ios".into(), "gone".into()]);
        assert_eq!(o, ["ios", "api", "web"]);
        let moved = |name: &str, earlier: bool| {
            let mut o = o.clone();
            move_in_order(&mut o, name, earlier);
            o
        };
        assert_eq!(moved("api", true), ["api", "ios", "web"]);
        assert_eq!(moved("web", false), o, "already last");
        assert_eq!(moved("ios", true), o, "already first");
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
        let prs = create_linked_prs(&ws, &dir, &["ops".into()], &NewPr::new("", false)).unwrap();
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
        let urls = create_linked_prs(&selected, &dir, &[], &NewPr::new("Scoped", true)).unwrap();
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
