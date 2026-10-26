//! Coordinated PRs: one per repo with commits, each body linking all the others.
use crate::git::{cmd_bounded, dirty, git};
use crate::state::Workspace;
use std::path::Path;

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
    let dirty: Vec<&str> = ws
        .repos
        .iter()
        .filter(|r| dirty(&dir.join(&r.name)).unwrap_or(false))
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
        let branch = ws.branch_of(r);
        cmd_bounded(&wt, "git", &["push", "-u", "origin", branch], 120)?;
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

/// gh, or `SHARK_GH` (tests point it at a fake, as `SHARK_CLAUDE` for claude).
fn gh() -> String {
    std::env::var("SHARK_GH").unwrap_or_else(|_| "gh".into())
}

/// Runs gh to post something; an error is gh's last word, not the command line (which carries
/// the whole body).
fn gh_post(wt: &Path, args: &[&str]) -> Result<String, String> {
    let gh = gh();
    cmd_bounded(wt, &gh, args, 60).map_err(|e| gh_error(&e, &format!("{gh} {}: ", args.join(" "))))
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
    let out = gh_post(wt, &["pr", "comment", &n, "--body", body])?;
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
    gh_post(wt, &args)
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
        let args = review_api_args("128", "abc", "app.rs:631-632", Some((&a, "Keep it aligned.")));
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

        // A failing create surfaces gh's error.
        fs::write(&fail, "").unwrap();
        let err =
            create_linked_prs(&ws, &dir, &["ops".into()], &NewPr::new("", false)).unwrap_err();
        assert!(err.contains("gh: boom"), "{err}");
        fs::remove_dir_all(&root).unwrap();
    }
}
