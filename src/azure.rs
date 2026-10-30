//! Azure DevOps through `az` (with its azure-devops extension): the PR lifecycle `gh` gives
//! GitHub repos, mapped onto the same types, plus the work items (PBIs and their tasks) a
//! branch is for. A repo is on Azure DevOps when its origin is; nothing is configured.
use crate::git::{cmd_bounded, git};
use crate::pr::git_feature::{Comment, Details, Review, Target, Thread};
use crate::sync::{Checks, Pr, PrState};
use serde_json::{Value, json};
use std::path::Path;

/// One Azure DevOps git repository, names decoded (`My Project`, not `My%20Project`).
#[derive(Clone, Debug, PartialEq)]
pub struct Remote {
    pub org: String,
    pub project: String,
    pub repo: String,
}

impl Remote {
    /// Its https, `user@` https, legacy `*.visualstudio.com` or ssh (`v3/`) clone URL.
    pub fn parse(url: &str) -> Option<Remote> {
        let url = url.trim().trim_end_matches('/');
        let new = |org: &str, project: &str, repo: &str| {
            let ok = |s: &str| !s.is_empty() && !s.contains(['/', '?', '#']);
            (ok(org) && ok(project) && ok(repo)).then(|| Remote {
                org: decode(org),
                project: decode(project),
                repo: decode(repo),
            })
        };
        if let Some((host, path)) = url.split_once("v3/") {
            if !host.contains("ssh.dev.azure.com") && !host.contains("vs-ssh.visualstudio.com") {
                return None;
            }
            return match path.split('/').collect::<Vec<_>>()[..] {
                [org, project, repo] => new(org, project, repo),
                _ => None,
            };
        }
        let rest = url.strip_prefix("https://")?;
        let (host, path) = rest.split_once('/')?;
        let host = host.rsplit('@').next()?.to_ascii_lowercase();
        let mut p: Vec<&str> = path.split('/').collect();
        let org = match host.strip_suffix(".visualstudio.com") {
            Some(org) => org.to_string(),
            None if host == "dev.azure.com" => p.remove(0).to_string(),
            None => return None,
        };
        if p.first()
            .is_some_and(|s| s.eq_ignore_ascii_case("DefaultCollection"))
        {
            p.remove(0);
        }
        match p[..] {
            [project, "_git", repo] => new(&org, project, repo),
            // A project's default repo may leave the project out.
            ["_git", repo] => new(&org, repo, repo),
            _ => None,
        }
    }

    /// The repo's origin, when that is on Azure DevOps.
    pub fn of(wt: &Path) -> Option<Remote> {
        Remote::parse(&git(wt, &["config", "--get", "remote.origin.url"]).ok()?)
    }

    fn org_url(&self) -> String {
        format!("https://dev.azure.com/{}", self.org)
    }

    /// The repo's web (and https clone) URL.
    pub fn web(&self) -> String {
        format!(
            "https://dev.azure.com/{}/{}/_git/{}",
            encode(&self.org),
            encode(&self.project),
            encode(&self.repo)
        )
    }

    pub fn pr_url(&self, number: u64) -> String {
        format!("{}/pullrequest/{number}", self.web())
    }

    pub fn branch_url(&self, branch: &str) -> String {
        format!("{}?version=GB{}", self.web(), encode(branch))
    }

    fn work_item_url(&self, id: u64) -> String {
        format!(
            "https://dev.azure.com/{}/{}/_workitems/edit/{id}",
            encode(&self.org),
            encode(&self.project)
        )
    }

    /// `az <args> --org … -o json`; `Null` for an empty answer.
    fn az(&self, wt: &Path, args: &[&str], secs: u64) -> Result<Value, String> {
        let org = self.org_url();
        let mut a = args.to_vec();
        a.extend(["--org", &org, "-o", "json", "--only-show-errors"]);
        let az = az();
        let out = cmd_bounded(wt, &az, &a, secs).map_err(|e| az_error(&e, &az, &a))?;
        match out.is_empty() {
            true => Ok(Value::Null),
            false => serde_json::from_str(&out).map_err(|e| format!("az: {e}")),
        }
    }

    /// A REST call on this repo through `az devops invoke`. Text goes in a JSON file, never on
    /// the command line, where a body starting with `-` would read as a flag.
    fn invoke(
        &self,
        wt: &Path,
        method: &str,
        resource: &str,
        route: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<Value, String> {
        let routes: Vec<String> = [("project", self.project.clone())]
            .into_iter()
            .chain([("repositoryId", self.repo.clone())])
            .chain(route.iter().cloned())
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        let mut args = vec![
            "devops",
            "invoke",
            "--area",
            "git",
            "--resource",
            resource,
            "--http-method",
            method,
            "--api-version",
            "7.1",
            "--route-parameters",
        ];
        args.extend(routes.iter().map(String::as_str));
        let file = body.map(BodyFile::new).transpose()?;
        if let Some(f) = &file {
            args.extend(["--in-file", &f.0]);
        }
        let v = self.az(wt, &args, 60);
        match (v, method) {
            (Err(e), "GET") => Err(e),
            // A lost answer may still have written.
            (Err(e), _) if e.contains("timed out") => Err(format!(
                "{e}; it may have gone through, so check Azure DevOps before retrying"
            )),
            (v, _) => v,
        }
    }

    fn get_pr(&self, wt: &Path, number: u64) -> Result<Value, String> {
        let id = ("pullRequestId", number.to_string());
        self.invoke(wt, "GET", "pullRequests", &[id], None)
    }

    fn patch_pr(&self, wt: &Path, number: u64, body: Value) -> Result<Value, String> {
        let id = ("pullRequestId", number.to_string());
        self.invoke(wt, "PATCH", "pullRequests", &[id], Some(&body))
    }
}

/// The request body of one `az devops invoke`, removed afterwards.
struct BodyFile(String);

impl BodyFile {
    fn new(body: &Value) -> Result<BodyFile, String> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("shark-az-{}-{n}.json", std::process::id()));
        std::fs::write(&path, body.to_string()).map_err(|e| e.to_string())?;
        Ok(BodyFile(path.to_string_lossy().into_owned()))
    }
}

impl Drop for BodyFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// az, or `SHARK_AZ` (tests point it at a fake, as `SHARK_GH` for gh).
pub(crate) fn az() -> String {
    std::env::var("SHARK_AZ").unwrap_or_else(|_| "az".into())
}

/// az's last word, without the command line; a missing login says how to fix it.
fn az_error(e: &str, az: &str, args: &[&str]) -> String {
    let why = crate::pr::gh_error(e, &format!("{az} {}: ", args.join(" ")));
    match why {
        _ if why.contains("az login") || why.contains("Before you can run Azure DevOps") => {
            "not logged in to Azure DevOps: run az login".into()
        }
        _ if why.contains("'devops' is misspelled") || why.contains("az extension add") => {
            "the azure-devops extension is missing: az extension add --name azure-devops".into()
        }
        _ => why,
    }
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = (b[i] == b'%')
            .then(|| s.get(i + 1..i + 3))
            .flatten()
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(h) => {
                out.push(h);
                i += 3;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Enough for URLs built from names Azure DevOps allows: spaces and the few reserved characters.
fn encode(s: &str) -> String {
    s.replace('%', "%25")
        .replace(' ', "%20")
        .replace('#', "%23")
        .replace('?', "%3F")
}

fn str<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or_default()
}

fn branch_name(r: &str) -> &str {
    r.strip_prefix("refs/heads/").unwrap_or(r)
}

/// `2026-10-21T14:46:06.4635+00:00` → `2026-10-21T14:46:06Z`, as GitHub writes them.
fn timestamp(s: &str) -> String {
    match s.get(..19) {
        Some(t) => format!("{t}Z"),
        None => s.into(),
    }
}

// ---- The git button: status, as `sync::status` reads it from gh ----

/// This branch's PR: the active one, else the newest. Fork PRs are ignored, as with gh.
pub fn fetch_pr(wt: &Path, remote: &Remote, branch: &str) -> Result<Option<Pr>, String> {
    let source = format!("refs/heads/{branch}");
    let list = remote.az(
        wt,
        &[
            "repos",
            "pr",
            "list",
            "--project",
            &remote.project,
            "--repository",
            &remote.repo,
            "--source-branch",
            &source,
            "--status",
            "all",
            "--top",
            "50",
        ],
        20,
    )?;
    let own: Vec<&Value> = (list.as_array().into_iter().flatten())
        .filter(|p| p["forkSource"].is_null())
        .collect();
    let Some(pick) = (own.iter().find(|p| p["status"] == "active").copied())
        .or_else(|| own.iter().copied().max_by_key(|p| str(p, "creationDate")))
    else {
        return Ok(None);
    };
    status(wt, remote, pick).map(Some)
}

/// The PR at `url` (a review checkout's), with its policies when it is open.
pub fn pr_at(wt: &Path, url: &str) -> Result<Pr, String> {
    let (remote, number) = parse_pr_url(url).ok_or("Invalid Azure DevOps pull request URL")?;
    status(wt, &remote, &remote.get_pr(wt, number)?)
}

/// `https://dev.azure.com/o/p/_git/r/pullrequest/7` → its repo and number.
pub fn parse_pr_url(url: &str) -> Option<(Remote, u64)> {
    let (repo, n) = url.trim_end_matches('/').rsplit_once("/pullrequest/")?;
    Some((Remote::parse(repo)?, n.parse().ok().filter(|n| *n > 0)?))
}

/// `pr` as listed, then, while it is open, what its policies and threads say.
fn status(wt: &Path, remote: &Remote, pr: &Value) -> Result<Pr, String> {
    let mut out = pr_from(remote, pr);
    if out.state != PrState::Open {
        return Ok(out);
    }
    let n = out.number.to_string();
    // Two independent calls; together they cost one az start-up instead of two.
    let (policies, threads) = std::thread::scope(|s| {
        let p = s.spawn(|| remote.az(wt, &["repos", "pr", "policy", "list", "--id", &n], 20));
        let id = ("pullRequestId", n.clone());
        let t = remote.invoke(wt, "GET", "pullRequestThreads", &[id], None);
        (p.join().unwrap_or_else(|_| Err("az panicked".into())), t)
    });
    apply_policies(&mut out, &policies?);
    apply_threads(&mut out, pr, &threads?["value"]);
    Ok(out)
}

/// What the PR itself says, mapped onto GitHub's vocabulary that `next_action` reads.
fn pr_from(remote: &Remote, p: &Value) -> Pr {
    let number = p["pullRequestId"].as_u64().unwrap_or(0);
    let reviewers: Vec<&Value> = p["reviewers"].as_array().into_iter().flatten().collect();
    let vote = |r: &Value| r["vote"].as_i64().unwrap_or(0);
    // -5 waiting for the author, -10 rejected: both ask for changes until the reviewer re-votes.
    let review = match () {
        _ if reviewers.iter().any(|r| vote(r) < 0) => "CHANGES_REQUESTED",
        _ if reviewers.iter().any(|r| vote(r) > 0) => "APPROVED",
        _ => "",
    };
    Pr {
        url: remote.pr_url(number),
        review_checkout: false,
        number,
        state: match str(p, "status") {
            "completed" => PrState::Merged,
            "abandoned" => PrState::Closed,
            _ => PrState::Open,
        },
        draft: p["isDraft"].as_bool().unwrap_or(false),
        // Azure DevOps doesn't require an up-to-date branch, so never BEHIND. `notSet` lasts
        // until something asks for a merge; it isn't "still computing".
        merge_state: match str(p, "mergeStatus") {
            "conflicts" => "DIRTY",
            "queued" => "UNKNOWN",
            "failure" | "rejectedByPolicy" => "BLOCKED",
            _ => "CLEAN",
        }
        .into(),
        review: review.into(),
        checks: Checks::Passing,
        head: str(&p["lastMergeSourceCommit"], "commitId").into(),
        base: branch_name(str(p, "targetRefName")).into(),
        // The vote is in the ID: a new rejection after a re-review is new work.
        change_requests: (reviewers.iter())
            .filter(|r| vote(r) < 0)
            .map(|r| format!("az-vote-{number}-{}-{}", str(r, "id"), vote(r)))
            .collect(),
        comments: vec![],
        contains_head: String::new(),
    }
}

/// Build and status policies are the checks; any other blocking policy that hasn't passed
/// (reviewers, linked work items, resolved comments) blocks the merge.
fn apply_policies(pr: &mut Pr, evaluations: &Value) {
    let (mut pending, mut failed, mut blocked, mut reviewers) = (false, vec![], false, false);
    for e in evaluations.as_array().into_iter().flatten() {
        let cfg = &e["configuration"];
        if cfg["isEnabled"] == false {
            continue;
        }
        let blocking = cfg["isBlocking"] != false;
        let settings = &cfg["settings"];
        let status = str(e, "status");
        let passed = matches!(status, "approved" | "notApplicable");
        match str(&cfg["type"], "displayName") {
            kind @ ("Build" | "Status") => {
                let name = [
                    settings["displayName"].as_str(),
                    settings["statusName"].as_str(),
                ]
                .into_iter()
                .flatten()
                .find(|s| !s.is_empty())
                .map(String::from)
                .unwrap_or_else(|| match settings["buildDefinitionId"].as_u64() {
                    Some(id) => format!("{kind} {id}"),
                    None => kind.into(),
                });
                match status {
                    "queued" | "running" => pending = true,
                    "rejected" | "broken" => failed.push(name),
                    _ => {}
                }
                blocked |= blocking && !passed;
            }
            "Minimum number of reviewers" | "Required reviewers" if blocking && !passed => {
                reviewers = true;
                blocked = true;
            }
            _ => blocked |= blocking && !passed,
        }
    }
    pr.checks = match (failed.is_empty(), pending) {
        (false, _) => Checks::Failing(failed),
        (true, true) => Checks::Pending,
        (true, false) => Checks::Passing,
    };
    if reviewers && pr.review != "CHANGES_REQUESTED" {
        pr.review = "REVIEW_REQUIRED".into();
    }
    if blocked && pr.merge_state == "CLEAN" {
        pr.merge_state = "BLOCKED".into();
    }
}

/// Active threads whose last word isn't the author's ask for work. The last comment is in the
/// ID, so a reviewer's new reply counts again once the agent has seen the old one.
fn apply_threads(pr: &mut Pr, p: &Value, threads: &Value) {
    let author = str(&p["createdBy"], "id");
    for t in threads.as_array().into_iter().flatten() {
        if t["isDeleted"] == true || !matches!(str(t, "status"), "active" | "pending") {
            continue;
        }
        let last = text_comments(t).last().copied();
        if let Some(c) = last.filter(|c| str(&c["author"], "id") != author) {
            pr.comments
                .push(format!("az-thread-{}-{}", t["id"], c["id"]));
        }
    }
}

/// A thread's human comments; Azure DevOps logs votes, pushes and the like as system ones.
fn text_comments(t: &Value) -> Vec<&Value> {
    (t["comments"].as_array().into_iter().flatten())
        .filter(|c| c["commentType"] == "text" && c["isDeleted"] != true)
        .collect()
}

/// For `#` completion and the new-workspace PR list, as `gh pr list --json` gives them.
pub fn list_prs(
    src: &Path,
    remote: &Remote,
    open: bool,
) -> Result<Vec<crate::complete::Pr>, String> {
    let (status, top) = if open {
        ("active", "1000")
    } else {
        ("all", "50")
    };
    let list = remote.az(
        src,
        &[
            "repos",
            "pr",
            "list",
            "--project",
            &remote.project,
            "--repository",
            &remote.repo,
            "--status",
            status,
            "--top",
            top,
        ],
        15,
    )?;
    Ok((list.as_array().into_iter().flatten())
        .map(|p| {
            let n = p["pullRequestId"].as_u64().unwrap_or(0);
            let updated = p["closedDate"].as_str().unwrap_or(str(p, "creationDate"));
            crate::complete::Pr {
                repo: String::new(),
                number: n,
                title: str(p, "title").into(),
                author: Some(crate::complete::Author {
                    login: str(&p["createdBy"], "displayName").into(),
                }),
                head_ref_name: branch_name(str(p, "sourceRefName")).into(),
                base_ref_name: branch_name(str(p, "targetRefName")).into(),
                url: remote.pr_url(n),
                state: match str(p, "status") {
                    "completed" => "MERGED",
                    "abandoned" => "CLOSED",
                    _ => "OPEN",
                }
                .into(),
                is_draft: p["isDraft"].as_bool().unwrap_or(false),
                updated_at: timestamp(updated),
                is_cross_repository: !p["forkSource"].is_null(),
            }
        })
        .collect())
}

// ---- The git button: steps ----

/// Opens a PR from `branch` into `base`, or returns the open one; its URL. Work items the
/// branch names (`feature/123-x`) are linked when they exist, as branch policies often want.
pub fn find_or_create_pr(
    wt: &Path,
    remote: &Remote,
    branch: &str,
    base: &str,
    new: &crate::pr::NewPr,
) -> Result<String, String> {
    let source = format!("refs/heads/{branch}");
    let open = remote.az(
        wt,
        &[
            "repos",
            "pr",
            "list",
            "--project",
            &remote.project,
            "--repository",
            &remote.repo,
            "--source-branch",
            &source,
            "--status",
            "active",
        ],
        30,
    )?;
    if let Some(n) = (open.as_array().into_iter().flatten())
        .find(|p| p["forkSource"].is_null())
        .and_then(|p| p["pullRequestId"].as_u64())
    {
        return Ok(remote.pr_url(n));
    }
    let (title, body) = match new.title.is_empty() {
        true => fill(wt, branch, base),
        false => (new.title.clone(), new.body.clone()),
    };
    let ids = branch_ids(branch);
    // A guess that doesn't exist would fail the whole PR; check it first, and skip on doubt.
    let linked: Vec<u64> = match ids.is_empty() {
        true => vec![],
        false => query(wt, remote, &format!("[System.Id] IN ({})", join(&ids)))
            .map(|items| items.iter().filter_map(|w| w["id"].as_u64()).collect())
            .unwrap_or_default(),
    };
    let created = remote.invoke(
        wt,
        "POST",
        "pullRequests",
        &[],
        Some(&json!({
            "sourceRefName": source,
            "targetRefName": format!("refs/heads/{base}"),
            "title": title,
            "description": body,
            "isDraft": new.draft,
            "workItemRefs": linked.iter().map(|id| json!({"id": id.to_string()})).collect::<Vec<_>>(),
        })),
    )?;
    let n = created["pullRequestId"]
        .as_u64()
        .ok_or("Azure DevOps didn't confirm the new PR; check it before retrying")?;
    Ok(remote.pr_url(n))
}

/// What `gh pr create --fill` would write: one commit's subject and body, else the branch name
/// and a list of the commit subjects.
fn fill(wt: &Path, branch: &str, base: &str) -> (String, String) {
    let range = format!("origin/{base}..HEAD");
    let subjects = git(wt, &["log", "--reverse", "--format=%s", &range]).unwrap_or_default();
    match subjects.lines().count() {
        1 => (
            subjects,
            git(wt, &["log", "-1", "--format=%b"]).unwrap_or_default(),
        ),
        _ => (
            branch
                .rsplit('/')
                .next()
                .unwrap_or(branch)
                .replace(['-', '_'], " "),
            subjects.lines().map(|s| format!("- {s}\n")).collect(),
        ),
    }
}

pub fn description(wt: &Path, url: &str) -> Result<String, String> {
    let (remote, n) = parse_pr_url(url).ok_or("Invalid Azure DevOps pull request URL")?;
    Ok(str(&remote.get_pr(wt, n)?, "description").into())
}

/// Updates the PR at `url` with `body`'s fields (`description`, `status`, `isDraft`, `targetRefName`).
pub fn update(wt: &Path, url: &str, body: Value) -> Result<(), String> {
    let (remote, n) = parse_pr_url(url).ok_or("Invalid Azure DevOps pull request URL")?;
    remote.patch_pr(wt, n, body).map(|_| ())
}

pub fn retarget(wt: &Path, url: &str, base: &str) -> Result<(), String> {
    update(
        wt,
        url,
        json!({"targetRefName": format!("refs/heads/{base}")}),
    )
}

/// Completes the PR at exactly `head`: Azure DevOps refuses when the source moved since, like
/// `gh pr merge --match-head-commit`. Squash unless the branch's policy rules it out.
pub fn merge(wt: &Path, url: &str, head: &str, delete_branch: bool) -> Result<(), String> {
    let (remote, n) = parse_pr_url(url).ok_or("Invalid Azure DevOps pull request URL")?;
    let pr = remote.get_pr(wt, n)?;
    let strategy = merge_strategy(wt, &remote, &pr);
    let done = remote.patch_pr(
        wt,
        n,
        json!({
            "status": "completed",
            "lastMergeSourceCommit": {"commitId": head},
            "completionOptions": {"mergeStrategy": strategy, "deleteSourceBranch": delete_branch},
        }),
    )?;
    match str(&done, "status") {
        "completed" | "active" => Ok(()),
        s => Err(format!("Azure DevOps left the PR {s}")),
    }
}

/// The first merge type the target branch's "Require a merge strategy" policy allows,
/// preferring squash; squash when there's no such policy or it can't be read.
fn merge_strategy(wt: &Path, remote: &Remote, pr: &Value) -> &'static str {
    let policies = remote.az(
        wt,
        &[
            "repos",
            "policy",
            "list",
            "--project",
            &remote.project,
            "--repository-id",
            str(&pr["repository"], "id"),
            "--branch",
            str(pr, "targetRefName"),
        ],
        20,
    );
    let Some(s) = (policies.ok().as_ref())
        .and_then(Value::as_array)
        .and_then(|ps| {
            ps.iter().find(|p| {
                p["isEnabled"] != false
                    && str(&p["type"], "displayName") == "Require a merge strategy"
            })
        })
        .map(|p| p["settings"].clone())
    else {
        return "squash";
    };
    [
        ("allowSquash", "squash"),
        ("allowNoFastForward", "noFastForward"),
        ("allowRebase", "rebase"),
        ("allowRebaseMerge", "rebaseMerge"),
    ]
    .into_iter()
    .find(|(k, _)| s[k] == true)
    .map_or("squash", |(_, v)| v)
}

/// Posts `body` as a new thread on PR `number`, on `inline`'s lines when given; its URL.
pub fn publish(
    wt: &Path,
    remote: &Remote,
    number: u64,
    body: &str,
    inline: Option<(&crate::pr::Anchor, &str)>,
) -> Result<String, String> {
    let mut comments = vec![json!({"parentCommentId": 0, "content": body, "commentType": 1})];
    let mut thread = json!({"status": 1});
    if let Some((a, text)) = inline {
        let side = if a.left { "left" } else { "right" };
        let start = a.start.unwrap_or(a.line);
        thread["threadContext"] = json!({
            "filePath": format!("/{}", a.path),
            format!("{side}FileStart"): {"line": start, "offset": 1},
            format!("{side}FileEnd"): {"line": a.line, "offset": 1},
        });
        // The note opens the thread; the comment on the lines replies under it.
        comments[0]["content"] = json!(text);
        if !body.trim().is_empty() && body.trim() != text.trim() {
            comments[0]["content"] = json!(format!("{}\n\n{text}", body.trim()));
        }
    }
    thread["comments"] = json!(comments);
    let id = ("pullRequestId", number.to_string());
    let t = remote.invoke(wt, "POST", "pullRequestThreads", &[id], Some(&thread))?;
    let tid = t["id"]
        .as_u64()
        .ok_or("Azure DevOps didn't confirm the comment; check the PR before retrying")?;
    Ok(format!("{}?discussionId={tid}", remote.pr_url(number)))
}

// ---- The Git artifact: one PR's conversation ----

impl Target {
    pub fn azure(remote: &Remote, number: u64) -> Target {
        Target {
            host: "dev.azure.com".into(),
            owner: format!("{}/{}", encode(&remote.org), encode(&remote.project)),
            repo: encode(&remote.repo),
            number,
        }
    }

    pub fn remote(&self) -> Option<Remote> {
        (self.host == "dev.azure.com")
            .then(|| {
                Remote::parse(&format!(
                    "https://dev.azure.com/{}/_git/{}",
                    self.owner, self.repo
                ))
            })
            .flatten()
    }
}

pub fn discover(wt: &Path, remote: &Remote, branch: &str) -> Result<Option<Target>, String> {
    Ok(fetch_pr(wt, remote, branch)?.map(|p| Target::azure(remote, p.number)))
}

const RESOLVED: [&str; 4] = ["fixed", "wontFix", "closed", "byDesign"];

pub fn load(wt: &Path, target: &Target) -> Result<Details, String> {
    let remote = target.remote().ok_or("Invalid Azure DevOps pull request")?;
    let pr = remote.get_pr(wt, target.number)?;
    let mut d = details_from(target, &pr)?;
    let id = ("pullRequestId", target.number.to_string());
    let threads = remote.invoke(wt, "GET", "pullRequestThreads", &[id], None)?;
    let vote_label = |v: i64| match v {
        10 => "APPROVED",
        5 => "Approved with suggestions",
        -5 => "Waiting for author",
        _ => "Rejected",
    };
    d.reviews = (pr["reviewers"].as_array().into_iter().flatten())
        .filter(|r| r["vote"].as_i64().is_some_and(|v| v != 0))
        .map(|r| Review {
            comment: Comment {
                id: format!("vote-{}", str(r, "id")),
                body: String::new(),
                author: str(r, "displayName").into(),
                url: String::new(),
                created_at: String::new(),
            },
            state: vote_label(r["vote"].as_i64().unwrap_or(0)).into(),
        })
        .collect();
    for t in threads["value"].as_array().into_iter().flatten() {
        let comments = text_comments(t);
        if t["isDeleted"] == true || comments.is_empty() {
            continue;
        }
        let tid = t["id"].as_u64().unwrap_or(0);
        let ctx = &t["threadContext"];
        let line = [&ctx["rightFileStart"], &ctx["leftFileStart"]]
            .iter()
            .find_map(|s| s["line"].as_u64());
        d.threads.push(Thread {
            id: tid.to_string(),
            path: str(ctx, "filePath").trim_start_matches('/').into(),
            line,
            resolved: RESOLVED.contains(&str(t, "status")),
            outdated: false,
            can_reply: true,
            can_resolve: true,
            can_unresolve: true,
            comments: comments
                .iter()
                .map(|c| Comment {
                    id: format!("{tid}-{}", c["id"]),
                    body: str(c, "content").into(),
                    author: str(&c["author"], "displayName").into(),
                    url: format!("{}?discussionId={tid}", d.url),
                    created_at: timestamp(str(c, "publishedDate")),
                })
                .collect(),
        });
    }
    Ok(d)
}

fn details_from(target: &Target, pr: &Value) -> Result<Details, String> {
    if pr["pullRequestId"].as_u64() != Some(target.number) {
        return Err(
            "Azure DevOps returned a different pull request; refresh before continuing".into(),
        );
    }
    Ok(Details {
        target: target.clone(),
        id: target.number.to_string(),
        title: str(pr, "title").into(),
        body: str(pr, "description").into(),
        url: target.url(),
        state: match str(pr, "status") {
            "completed" => "MERGED",
            "abandoned" => "CLOSED",
            _ => "OPEN",
        }
        .into(),
        author: str(&pr["createdBy"], "displayName").into(),
        head: branch_name(str(pr, "sourceRefName")).into(),
        base: branch_name(str(pr, "targetRefName")).into(),
        draft: pr["isDraft"].as_bool().unwrap_or(false),
        // ponytail: az can't ask for permissions up front; a refused write says so instead.
        can_update: true,
        can_comment: true,
        comments: vec![],
        reviews: vec![],
        threads: vec![],
    })
}

/// Saves only over the version the draft started from; Azure DevOps has no compare-and-swap
/// either, so the same small window as on GitHub remains.
pub fn save_description(
    wt: &Path,
    previous: &Details,
    title: &str,
    body: &str,
) -> Result<(), String> {
    let remote = previous
        .target
        .remote()
        .ok_or("Invalid Azure DevOps pull request")?;
    let current = details_from(
        &previous.target,
        &remote.get_pr(wt, previous.target.number)?,
    )?;
    if current.title != previous.title || current.body != previous.body {
        return Err("The PR description changed on Azure DevOps. Your draft is kept; refresh and compare before saving.".into());
    }
    remote
        .patch_pr(
            wt,
            previous.target.number,
            json!({"title": title, "description": body}),
        )
        .map(|_| ())
}

pub fn comment(wt: &Path, previous: &Details, body: &str) -> Result<(), String> {
    let remote = previous
        .target
        .remote()
        .ok_or("Invalid Azure DevOps pull request")?;
    publish(wt, &remote, previous.target.number, body, None).map(|_| ())
}

fn fresh_thread(wt: &Path, previous: &Details, id: &str) -> Result<(Remote, Value), String> {
    if !previous.threads.iter().any(|t| t.id == id) {
        return Err("This thread does not belong to the displayed pull request".into());
    }
    let remote = previous
        .target
        .remote()
        .ok_or("Invalid Azure DevOps pull request")?;
    let route = [
        ("pullRequestId", previous.target.number.to_string()),
        ("threadId", id.to_string()),
    ];
    let t = remote.invoke(wt, "GET", "pullRequestThreads", &route, None)?;
    if t["id"].as_u64().map(|n| n.to_string()).as_deref() != Some(id) || t["isDeleted"] == true {
        return Err("Review thread is no longer available; refresh before continuing".into());
    }
    Ok((remote, t))
}

pub fn reply(wt: &Path, previous: &Details, thread_id: &str, body: &str) -> Result<(), String> {
    let (remote, t) = fresh_thread(wt, previous, thread_id)?;
    let first = text_comments(&t)
        .first()
        .and_then(|c| c["id"].as_u64())
        .unwrap_or(1);
    let route = [
        ("pullRequestId", previous.target.number.to_string()),
        ("threadId", thread_id.to_string()),
    ];
    let body = json!({"parentCommentId": first, "content": body, "commentType": 1});
    let c = remote.invoke(wt, "POST", "pullRequestThreadComments", &route, Some(&body))?;
    match c["id"].as_u64() {
        Some(_) => Ok(()),
        None => {
            Err("Azure DevOps did not confirm the reply. Refresh and check before retrying.".into())
        }
    }
}

pub fn resolve(
    wt: &Path,
    previous: &Details,
    thread_id: &str,
    resolved: bool,
) -> Result<(), String> {
    let (remote, t) = fresh_thread(wt, previous, thread_id)?;
    if RESOLVED.contains(&str(&t, "status")) == resolved {
        return Ok(());
    }
    let route = [
        ("pullRequestId", previous.target.number.to_string()),
        ("threadId", thread_id.to_string()),
    ];
    let status = if resolved { "fixed" } else { "active" };
    remote
        .invoke(
            wt,
            "PATCH",
            "pullRequestThreads",
            &route,
            Some(&json!({"status": status})),
        )
        .map(|_| ())
}

// ---- Work items: the PBIs a branch is for, and their tasks ----

#[derive(Clone, Debug, PartialEq)]
pub struct WorkItem {
    pub id: u64,
    /// `Product Backlog Item`, `Task`, `Bug`, …
    pub kind: String,
    pub title: String,
    pub state: String,
    /// Display name; empty when unassigned.
    pub assigned: String,
    pub url: String,
    pub children: Vec<WorkItem>,
}

impl WorkItem {
    pub fn done(&self) -> bool {
        matches!(
            self.state.as_str(),
            "Done" | "Closed" | "Resolved" | "Removed" | "Completed"
        )
    }
}

/// The work items linked to PR `pr`, else the IDs in the branch name; each with its children.
/// Empty for repos that aren't on Azure DevOps.
pub fn work_items(wt: &Path, branch: &str, pr: Option<u64>) -> Result<Vec<WorkItem>, String> {
    let Some(remote) = Remote::of(wt) else {
        return Ok(vec![]);
    };
    let mut ids = vec![];
    if let Some(n) = pr {
        let id = ("pullRequestId", n.to_string());
        let linked = remote.invoke(wt, "GET", "pullRequestWorkItems", &[id], None)?;
        ids = (linked["value"].as_array().into_iter().flatten())
            .filter_map(|w| w["id"].as_str()?.parse().ok())
            .collect();
    }
    if ids.is_empty() {
        ids = branch_ids(branch);
    }
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let list = join(&ids);
    let rows = query(
        wt,
        &remote,
        &format!("[System.Id] IN ({list}) OR [System.Parent] IN ({list})"),
    )?;
    Ok(tree(&remote, &ids, &rows))
}

fn join(ids: &[u64]) -> String {
    ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",")
}

/// Work item IDs a branch name carries, as `feature/131035-x` or `pbi/x_131035`: runs of 4+
/// digits between separators.
pub fn branch_ids(branch: &str) -> Vec<u64> {
    let mut ids = vec![];
    for part in branch.split(|c: char| !c.is_ascii_alphanumeric()) {
        if part.len() >= 4
            && part.bytes().all(|b| b.is_ascii_digit())
            && let Ok(id) = part.parse()
            && !ids.contains(&id)
        {
            ids.push(id);
        }
    }
    ids
}

fn query(wt: &Path, remote: &Remote, filter: &str) -> Result<Vec<Value>, String> {
    let wiql = format!(
        "SELECT [System.Id],[System.Title],[System.State],[System.WorkItemType],[System.AssignedTo],[System.Parent] FROM WorkItems WHERE {filter} ORDER BY [System.Id]"
    );
    let v = remote.az(
        wt,
        &[
            "boards",
            "query",
            "--project",
            &remote.project,
            "--wiql",
            &wiql,
        ],
        30,
    )?;
    Ok(v.as_array().cloned().unwrap_or_default())
}

/// `ids` in their order, each with the rows whose parent it is.
fn tree(remote: &Remote, ids: &[u64], rows: &[Value]) -> Vec<WorkItem> {
    let item = |v: &Value| {
        let f = &v["fields"];
        let id = v["id"].as_u64().unwrap_or(0);
        WorkItem {
            id,
            kind: str(f, "System.WorkItemType").into(),
            title: str(f, "System.Title").into(),
            state: str(f, "System.State").into(),
            assigned: str(&f["System.AssignedTo"], "displayName").into(),
            url: remote.work_item_url(id),
            children: vec![],
        }
    };
    ids.iter()
        .filter_map(|id| rows.iter().find(|r| r["id"].as_u64() == Some(*id)))
        .map(|r| WorkItem {
            children: (rows.iter())
                .filter(|c| c["fields"]["System.Parent"].as_u64() == r["id"].as_u64())
                .map(item)
                .collect(),
            ..item(r)
        })
        .collect()
}

/// For the workspace's CLAUDE.md: the commands the agent needs instead of gh.
pub const AGENT_HINT: &str = "Repos on Azure DevOps have no `gh`; use `az` (azure-devops extension) with `--org https://dev.azure.com/<org>`:
- the branch's PR: `az repos pr list --project <project> --repository <repo> --source-branch <branch> --status active`
- checks: `az repos pr policy list --id <pr>`; a build's logs: `az devops invoke --area build --resource logs --route-parameters project=<project> buildId=<id>` (add `logId=<n>` for one log's text)
- review comments: `az devops invoke --area git --resource pullRequestThreads --route-parameters project=<project> repositoryId=<repo> pullRequestId=<pr> --api-version 7.1`
- work items: `az boards work-item show --id <id>`\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_remote_form() {
        let r = |org: &str, project: &str, repo: &str| {
            Some(Remote {
                org: org.into(),
                project: project.into(),
                repo: repo.into(),
            })
        };
        for url in [
            "https://edc-group@dev.azure.com/edc-group/EDC/_git/EDC.Core",
            "https://dev.azure.com/edc-group/EDC/_git/EDC.Core/",
            "git@ssh.dev.azure.com:v3/edc-group/EDC/EDC.Core",
            "ssh://git@ssh.dev.azure.com/v3/edc-group/EDC/EDC.Core",
            "https://edc-group.visualstudio.com/EDC/_git/EDC.Core",
            "https://edc-group.visualstudio.com/DefaultCollection/EDC/_git/EDC.Core",
        ] {
            assert_eq!(
                Remote::parse(url),
                r("edc-group", "EDC", "EDC.Core"),
                "{url}"
            );
        }
        assert_eq!(
            Remote::parse("https://dev.azure.com/o/My%20Project/_git/My%20Project"),
            r("o", "My Project", "My Project")
        );
        assert_eq!(
            Remote::parse("https://dev.azure.com/o/_git/r"),
            r("o", "r", "r")
        );
        assert_eq!(Remote::parse("https://github.com/o/r.git"), None);
        assert_eq!(Remote::parse("git@github.com:o/r.git"), None);
        let remote = r("o", "My Project", "r").unwrap();
        let url = remote.pr_url(7);
        assert_eq!(
            url,
            "https://dev.azure.com/o/My%20Project/_git/r/pullrequest/7"
        );
        assert_eq!(parse_pr_url(&url), Some((remote.clone(), 7)));
        let target = Target::azure(&remote, 7);
        assert_eq!(target.url(), url, "drafts are keyed by this URL");
        assert_eq!(Target::from_url(&url), Ok(target.clone()));
        assert_eq!(target.remote(), Some(remote));
    }

    /// Read-only, against a real clone: `SHARK_AZURE_LIVE_REPO=<path> SHARK_AZURE_LIVE_BRANCH=<b>`.
    #[test]
    #[ignore]
    fn live_read_only() {
        let wt = std::path::PathBuf::from(std::env::var("SHARK_AZURE_LIVE_REPO").unwrap());
        let branch = std::env::var("SHARK_AZURE_LIVE_BRANCH").unwrap();
        let remote = Remote::of(&wt).expect("origin on Azure DevOps");
        let pr = fetch_pr(&wt, &remote, &branch)
            .unwrap()
            .expect("the branch has a PR");
        println!(
            "PR #{} {:?} draft={} merge={} review={} checks={:?} change_requests={} comments={}",
            pr.number,
            pr.state,
            pr.draft,
            pr.merge_state,
            pr.review,
            pr.checks,
            pr.change_requests.len(),
            pr.comments.len()
        );
        let target = discover(&wt, &remote, &branch).unwrap().unwrap();
        let d = load(&wt, &target).unwrap();
        println!(
            "details: state={} base={} reviews={} threads={} resolved={}",
            d.state,
            d.base,
            d.reviews.len(),
            d.threads.len(),
            d.threads.iter().filter(|t| t.resolved).count()
        );
        for w in work_items(&wt, &branch, Some(pr.number)).unwrap() {
            let kids: Vec<String> = (w.children.iter())
                .map(|c| format!("{} {}", c.kind, c.state))
                .collect();
            println!("work item {} {} {}: {kids:?}", w.id, w.kind, w.state);
        }
        let open = list_prs(&wt, &remote, true).unwrap();
        println!("open PRs: {}", open.len());
    }

    #[test]
    fn branch_names_carry_work_item_ids() {
        assert_eq!(branch_ids("feature/131035-search"), vec![131035]);
        assert_eq!(
            branch_ids("pbi/search_sold_cases_mm_.rest_131035"),
            vec![131035]
        );
        assert_eq!(branch_ids("fix/v2-123-abc4567"), Vec::<u64>::new());
    }

    fn remote() -> Remote {
        Remote::parse("https://dev.azure.com/o/p/_git/r").unwrap()
    }

    #[test]
    fn maps_an_open_pr_onto_githubs_vocabulary() {
        let pr = json!({
            "pullRequestId": 12, "status": "active", "isDraft": false, "mergeStatus": "succeeded",
            "lastMergeSourceCommit": {"commitId": "abc"}, "targetRefName": "refs/heads/main",
            "createdBy": {"id": "me"},
            "reviewers": [{"id": "rev", "vote": -5}, {"id": "ok", "vote": 10}],
        });
        let mut p = pr_from(&remote(), &pr);
        assert_eq!(
            (p.state, p.base.as_str(), p.head.as_str()),
            (PrState::Open, "main", "abc")
        );
        assert_eq!(p.url, "https://dev.azure.com/o/p/_git/r/pullrequest/12");
        assert_eq!(p.review, "CHANGES_REQUESTED");
        assert_eq!(p.change_requests, ["az-vote-12-rev--5"]);
        let policies = json!([
            {"status": "rejected", "configuration": {"isEnabled": true, "isBlocking": true,
                "type": {"displayName": "Build"}, "settings": {"buildDefinitionId": 7, "displayName": null}}},
            {"status": "running", "configuration": {"isEnabled": true, "isBlocking": false,
                "type": {"displayName": "Build"}, "settings": {"displayName": "lint"}}},
            {"status": "queued", "configuration": {"isEnabled": true, "isBlocking": true,
                "type": {"displayName": "Minimum number of reviewers"}, "settings": {}}},
            {"status": "rejected", "configuration": {"isEnabled": false, "isBlocking": true,
                "type": {"displayName": "Build"}, "settings": {"displayName": "off"}}},
        ]);
        apply_policies(&mut p, &policies);
        assert_eq!(p.checks, Checks::Failing(vec!["Build 7".into()]));
        assert_eq!(p.merge_state, "BLOCKED");
        assert_eq!(
            p.review, "CHANGES_REQUESTED",
            "a rejection outranks a missing approval"
        );
        let threads = json!([
            {"id": 1, "status": "active", "comments": [
                {"id": 1, "commentType": "text", "author": {"id": "me"}},
                {"id": 2, "commentType": "text", "author": {"id": "rev"}}]},
            {"id": 2, "status": "active", "comments": [
                {"id": 1, "commentType": "text", "author": {"id": "rev"}},
                {"id": 2, "commentType": "text", "author": {"id": "me"}}]},
            {"id": 3, "status": "fixed", "comments": [{"id": 1, "commentType": "text", "author": {"id": "rev"}}]},
            {"id": 4, "comments": [{"id": 1, "commentType": "system", "author": {"id": "rev"}}]},
        ]);
        apply_threads(&mut p, &pr, &threads);
        assert_eq!(
            p.comments,
            ["az-thread-1-2"],
            "only threads awaiting the author"
        );
        let clean = pr_from(
            &remote(),
            &json!({"pullRequestId": 1, "status": "completed", "mergeStatus": "notSet"}),
        );
        assert_eq!(
            (clean.state, clean.merge_state.as_str()),
            (PrState::Merged, "CLEAN")
        );
    }

    #[test]
    fn work_items_nest_their_children() {
        let row = |id: u64, kind: &str, state: &str, parent: Option<u64>| {
            json!({"id": id, "fields": {"System.WorkItemType": kind, "System.Title": format!("t{id}"),
                "System.State": state, "System.Parent": parent,
                "System.AssignedTo": {"displayName": "Dev"}}})
        };
        let rows = [
            row(1, "Product Backlog Item", "In Progress", Some(9)),
            row(2, "Task", "Done", Some(1)),
            row(3, "Task", "To Do", Some(1)),
        ];
        let items = tree(&remote(), &[1, 404], &rows);
        assert_eq!(items.len(), 1, "an ID that doesn't exist is dropped");
        let pbi = &items[0];
        assert_eq!(
            (pbi.kind.as_str(), pbi.state.as_str()),
            ("Product Backlog Item", "In Progress")
        );
        assert_eq!(pbi.url, "https://dev.azure.com/o/p/_workitems/edit/1");
        let kids: Vec<(u64, bool)> = pbi.children.iter().map(|c| (c.id, c.done())).collect();
        assert_eq!(kids, [(2, true), (3, false)]);
    }
}
