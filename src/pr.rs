//! Coordinated PRs: one per repo with commits, each body linking all the others.
use crate::git::{cmd_bounded, git};
use crate::state::Workspace;
use std::path::Path;

const MARKER: &str = "<!-- shark:related -->";

/// How new PRs are opened. The composer's first line is the title and the rest the body;
/// empty falls back to `--fill` (commit info). `--fill` stays on, so a title alone still gets
/// the commits as body: gh lets `--title`/`--body` override what it fills in.
#[derive(Default)]
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

/// `order` with `name` moved one place earlier (`earlier`) or later; unchanged at the ends.
pub fn move_in_order(mut order: Vec<String>, name: &str, earlier: bool) -> Vec<String> {
    if let Some(i) = order.iter().position(|n| n == name) {
        let j = if earlier { i.wrapping_sub(1) } else { i + 1 };
        if j < order.len() {
            order.swap(i, j);
        }
    }
    order
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
                let out = cmd_bounded(&wt, "gh", &new.args(&ws.branch, base), 60)?;
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
                })
                .into(),
            session_id: None,
            model: None,
            viewed: Default::default(),
        };
        let o = merge_order(&ws, &["ios".into(), "gone".into()]);
        assert_eq!(o, ["ios", "api", "web"]);
        assert_eq!(move_in_order(o.clone(), "api", true), ["api", "ios", "web"]);
        assert_eq!(move_in_order(o.clone(), "web", false), o, "already last");
        assert_eq!(move_in_order(o.clone(), "ios", true), o, "already first");
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
        let ws = create_workspace(&dir, "feat", "feat", &repos).unwrap();
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
        let err = create_linked_prs(&ws, &dir, &[], &NewPr::default()).unwrap_err();
        assert!(err.contains("docs"), "{err}");
        fs::remove_file(dir.join("docs/stray.txt")).unwrap();

        let prs = create_linked_prs(&ws, &dir, &["ops".into()], &NewPr::default()).unwrap();
        assert_eq!(
            prs,
            [
                ("api".to_string(), "https://gh/api/1".to_string()),
                ("web".into(), "https://gh/web/7".into())
            ]
        );
        assert_eq!(
            fs::read_to_string(&log).unwrap(),
            "api pr list\napi pr create --head feat --base main --fill\nweb pr list\napi pr view\napi pr edit\nweb pr view\nweb pr edit\n"
        );
        let body = fs::read_to_string(root.join("api.body")).unwrap();
        assert!(
            body.starts_with("Body\n\n") && body.contains("- web: https://gh/web/7"),
            "{body}"
        );
        let pushed = |name: &str| {
            git(
                &root.join(format!("{name}.git")),
                &["branch", "--list", "feat"],
            )
            .unwrap()
        };
        assert!(!pushed("api").is_empty() && pushed("ops").is_empty());

        // A failing create surfaces gh's error.
        fs::write(&fail, "").unwrap();
        let err = create_linked_prs(&ws, &dir, &["ops".into()], &NewPr::default()).unwrap_err();
        assert!(err.contains("gh: boom"), "{err}");
        fs::remove_dir_all(&root).unwrap();
    }
}
