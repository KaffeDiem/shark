//! PR conversations addressed by immutable repository/number, never a mutable branch name.
use super::Target;
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::HashSet,
    path::Path,
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct Comment {
    pub id: String,
    pub body: String,
    pub author: String,
    pub url: String,
    pub created_at: String,
    /// The author is a GitHub App or bot account.
    pub bot: bool,
    /// Hidden on GitHub (outdated, resolved, spam…); shown collapsed.
    pub minimized: bool,
    /// Review comments only: the diff context GitHub shows above a thread.
    pub diff_hunk: Option<String>,
    /// Review comments only: the review the comment was submitted with.
    pub review_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Review {
    pub comment: Comment,
    pub state: String,
}

#[derive(Clone, Debug)]
pub struct Thread {
    pub id: String,
    pub path: String,
    pub line: Option<u64>,
    pub resolved: bool,
    pub outdated: bool,
    pub can_reply: bool,
    pub can_resolve: bool,
    pub can_unresolve: bool,
    /// The line in the original diff; outdated threads have no current `line`.
    pub original_line: Option<u64>,
    pub resolved_by: Option<String>,
    pub comments: Vec<Comment>,
}

#[derive(Clone, Debug)]
pub struct Details {
    pub target: Target,
    pub id: String,
    pub title: String,
    pub body: String,
    pub url: String,
    pub state: String,
    pub author: String,
    pub created_at: String,
    pub head: String,
    /// The head commit when read, for resolving relative assets; `None` when GitHub omits it.
    pub head_sha: Option<String>,
    pub base: String,
    pub draft: bool,
    pub can_update: bool,
    pub can_comment: bool,
    pub comments: Vec<Comment>,
    pub reviews: Vec<Review>,
    pub threads: Vec<Thread>,
}

const INFO: &str = "id number url title body state isDraft createdAt author{login} headRefName headRefOid baseRefName viewerCanUpdate locked";
const COMMENT: &str = "id body author{__typename login} url createdAt isMinimized";
const REVIEW_COMMENT: &str =
    "id body author{__typename login} url createdAt isMinimized diffHunk pullRequestReview{id}";
const PAGE: &str = "pageInfo{hasNextPage endCursor}";
const THREAD: &str = "id path line originalLine isResolved isOutdated viewerCanReply viewerCanResolve viewerCanUnresolve resolvedBy{login}";

/// Reads are bounded as a whole, including pagination, as well as per subprocess.
struct Client<'a> {
    wt: &'a Path,
    target: &'a Target,
    started: Instant,
}

impl<'a> Client<'a> {
    fn new(wt: &'a Path, target: &'a Target) -> Self {
        Self {
            wt,
            target,
            started: Instant::now(),
        }
    }

    fn query(&self, query: &str, fields: &[(&str, &str)], mutation: bool) -> Result<Value, String> {
        let remaining = Duration::from_secs(120)
            .saturating_sub(self.started.elapsed())
            .as_secs();
        if remaining == 0 {
            return Err("GitHub request took too long; refresh to try again".into());
        }
        let mut args = vec![
            "api".into(),
            "graphql".into(),
            "--hostname".into(),
            self.target.host.clone(),
            "-f".into(),
            format!("query={query}"),
        ];
        for (key, value) in fields {
            args.extend(["-f".into(), format!("{key}={value}")]);
        }
        let refs: Vec<_> = args.iter().map(String::as_str).collect();
        let out = super::gh_post(self.wt, &refs, remaining.min(30)).map_err(|error| {
            if mutation && error.contains("timed out") {
                "GitHub timed out; the change may have been saved. Refresh and check before retrying.".into()
            } else { error }
        })?;
        let value: Value =
            serde_json::from_str(&out).map_err(|e| format!("Invalid GitHub response: {e}"))?;
        if value
            .get("errors")
            .and_then(Value::as_array)
            .is_some_and(|e| !e.is_empty())
        {
            return Err(format!(
                "GitHub: {}",
                value["errors"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|e| e["message"].as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        value
            .get("data")
            .cloned()
            .filter(|v| !v.is_null())
            .ok_or_else(|| "GitHub returned no data".into())
    }

    fn pr(&self, fields: &str) -> Result<Value, String> {
        let query = format!(
            "query($owner:String!,$repo:String!){{repository(owner:$owner,name:$repo){{pullRequest(number:{}){{{fields}}}}}}}",
            self.target.number
        );
        let data = self.query(
            &query,
            &[("owner", &self.target.owner), ("repo", &self.target.repo)],
            false,
        )?;
        let pr = &data["repository"]["pullRequest"];
        if pr.is_null() {
            return Err("Pull request is unavailable or you no longer have access".into());
        }
        Ok(pr.clone())
    }

    fn node(
        &self,
        id: &str,
        kind: &str,
        fields: &str,
        cursor: Option<&str>,
    ) -> Result<Value, String> {
        let cursor_decl = if fields.contains("$cursor") {
            ",$cursor:String"
        } else {
            ""
        };
        let query =
            format!("query($id:ID!{cursor_decl}){{node(id:$id){{... on {kind}{{{fields}}}}}}}");
        let mut vars = vec![("id", id)];
        if let Some(cursor) = cursor {
            vars.push(("cursor", cursor));
        }
        let data = self.query(&query, &vars, false)?;
        data.get("node")
            .cloned()
            .filter(|v| !v.is_null())
            .ok_or_else(|| "GitHub item is no longer available".into())
    }
}

pub fn discover(wt: &Path, branch: &str) -> Result<Option<Target>, String> {
    if let Some(url) = crate::git::review_pr_url(wt, branch) {
        return Target::from_url(&url).map(Some);
    }
    if let Some(remote) = crate::azure::Remote::of(wt) {
        return crate::azure::discover(wt, &remote, branch);
    }
    super::origin_repo(wt).ok_or("Origin is not a GitHub or Azure DevOps repository")?;
    let prs = super::branch_prs(wt, branch, "url,state,isCrossRepository,updatedAt", 30)?;
    super::pick_pr(&prs.iter().collect::<Vec<_>>())
        .map(|p| Target::from_url(string(p, "url")?))
        .transpose()
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("GitHub response is missing {key}"))
}

fn boolean(value: &Value, key: &str) -> Result<bool, String> {
    value[key]
        .as_bool()
        .ok_or_else(|| format!("GitHub response is missing {key}"))
}

fn author(value: &Value) -> String {
    value["author"]["login"].as_str().unwrap_or("ghost").into()
}

fn comment_from(value: &Value) -> Result<Comment, String> {
    Ok(Comment {
        id: string(value, "id")?.into(),
        body: string(value, "body")?.into(),
        author: author(value),
        url: string(value, "url")?.into(),
        created_at: string(value, "createdAt")?.into(),
        bot: value["author"]["__typename"].as_str() == Some("Bot"),
        minimized: value["isMinimized"].as_bool().unwrap_or(false),
        diff_hunk: value["diffHunk"].as_str().map(Into::into),
        review_id: value["pullRequestReview"]["id"].as_str().map(Into::into),
    })
}

fn details_from(target: &Target, value: &Value) -> Result<Details, String> {
    let url = string(value, "url")?;
    let returned = Target::from_url(url)?;
    if returned.host != target.host
        || !returned.owner.eq_ignore_ascii_case(&target.owner)
        || !returned.repo.eq_ignore_ascii_case(&target.repo)
        || returned.number != target.number
        || value["number"].as_u64() != Some(target.number)
    {
        return Err("GitHub returned a different pull request; refresh before continuing".into());
    }
    Ok(Details {
        target: target.clone(),
        id: string(value, "id")?.into(),
        title: string(value, "title")?.into(),
        body: string(value, "body")?.into(),
        url: url.into(),
        state: string(value, "state")?.into(),
        author: author(value),
        created_at: value["createdAt"].as_str().unwrap_or_default().into(),
        head: string(value, "headRefName")?.into(),
        head_sha: value["headRefOid"]
            .as_str()
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(Into::into),
        base: string(value, "baseRefName")?.into(),
        draft: boolean(value, "isDraft")?,
        can_update: boolean(value, "viewerCanUpdate")?,
        can_comment: !boolean(value, "locked")? || boolean(value, "viewerCanUpdate")?,
        comments: vec![],
        reviews: vec![],
        threads: vec![],
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

fn pages(
    client: &Client,
    id: &str,
    kind: &str,
    key: &str,
    fields: &str,
    first: Value,
) -> Result<Vec<Value>, String> {
    let mut page = first;
    let mut out = vec![];
    let mut cursors = HashSet::new();
    let mut ids = HashSet::new();
    loop {
        let nodes = page["nodes"]
            .as_array()
            .ok_or_else(|| format!("GitHub response is missing {key} nodes"))?;
        for node in nodes.iter().filter(|n| !n.is_null()) {
            if ids.insert(string(node, "id")?.to_string()) {
                out.push(node.clone());
            }
        }
        let info: PageInfo = serde_json::from_value(page["pageInfo"].clone())
            .map_err(|e| format!("Invalid GitHub pagination: {e}"))?;
        if !info.has_next_page {
            break;
        }
        let cursor = info
            .end_cursor
            .filter(|c| !c.is_empty())
            .ok_or("GitHub pagination cursor is missing")?;
        if !cursors.insert(cursor.clone()) {
            return Err("GitHub pagination did not advance; refresh to try again".into());
        }
        page = client.node(
            id,
            kind,
            &format!("{key}(first:100,after:$cursor){{nodes{{{fields}}}{PAGE}}}"),
            Some(&cursor),
        )?[key]
            .clone();
    }
    Ok(out)
}

/// Every comment of the node `id`, following pagination from its `first` page.
fn comments(
    client: &Client,
    id: &str,
    kind: &str,
    fields: &str,
    first: &Value,
) -> Result<Vec<Comment>, String> {
    pages(client, id, kind, "comments", fields, first.clone())?
        .iter()
        .map(comment_from)
        .collect()
}

pub fn load(wt: &Path, target: &Target) -> Result<Details, String> {
    if target.remote().is_some() {
        return crate::azure::load(wt, target);
    }
    let client = Client::new(wt, target);
    let review_fields = format!("{COMMENT} state");
    let thread_fields = format!("{THREAD} comments(first:100){{nodes{{{REVIEW_COMMENT}}}{PAGE}}}");
    let value = client.pr(&format!("{INFO} comments(first:100){{nodes{{{COMMENT}}}{PAGE}}} reviews(first:100){{nodes{{{review_fields}}}{PAGE}}} reviewThreads(first:100){{nodes{{{thread_fields}}}{PAGE}}}"))?;
    let mut details = details_from(target, &value)?;
    details.comments = comments(
        &client,
        &details.id,
        "PullRequest",
        COMMENT,
        &value["comments"],
    )?;
    details.reviews = pages(
        &client,
        &details.id,
        "PullRequest",
        "reviews",
        &review_fields,
        value["reviews"].clone(),
    )?
    .iter()
    .map(|v| {
        Ok(Review {
            comment: comment_from(v)?,
            state: string(v, "state")?.into(),
        })
    })
    .collect::<Result<_, String>>()?;
    for value in pages(
        &client,
        &details.id,
        "PullRequest",
        "reviewThreads",
        &thread_fields,
        value["reviewThreads"].clone(),
    )? {
        let id = string(&value, "id")?.to_string();
        let mut comments = comments(
            &client,
            &id,
            "PullRequestReviewThread",
            REVIEW_COMMENT,
            &value["comments"],
        )?;
        // GitHub lists a thread by `createdAt`, which a review comment takes from when its
        // review was submitted: a question from a review submitted late sorts below the
        // replies to it. Its URL's `discussion_r<id>` keeps the order it was written in.
        comments.sort_by_key(|c| discussion_id(&c.url));
        details.threads.push(Thread {
            id,
            path: string(&value, "path")?.into(),
            line: value["line"].as_u64(),
            resolved: boolean(&value, "isResolved")?,
            outdated: boolean(&value, "isOutdated")?,
            can_reply: boolean(&value, "viewerCanReply")?,
            can_resolve: boolean(&value, "viewerCanResolve")?,
            can_unresolve: boolean(&value, "viewerCanUnresolve")?,
            original_line: value["originalLine"].as_u64(),
            resolved_by: value["resolvedBy"]["login"].as_str().map(Into::into),
            comments,
        });
    }
    Ok(details)
}

/// The number in a review comment URL's `#discussion_r<n>`; comments without one keep
/// their place at the front.
fn discussion_id(url: &str) -> u64 {
    url.rsplit_once("#discussion_r")
        .and_then(|(_, n)| n.parse().ok())
        .unwrap_or(0)
}

fn fresh(client: &Client, previous: &Details) -> Result<Details, String> {
    let details = details_from(&previous.target, &client.pr(INFO)?)?;
    if details.id != previous.id {
        return Err("Pull request identity changed; refresh before continuing".into());
    }
    Ok(details)
}

/// Runs a mutation and requires GitHub to echo a non-empty id at the JSON pointer `id_path`.
fn mutate(
    client: &Client,
    query: &str,
    vars: &[(&str, &str)],
    id_path: &str,
) -> Result<(), String> {
    let data = client
        .query(query, vars, true)
        .map_err(|e| format!("{e} Refresh and check before retrying."))?;
    if data
        .pointer(id_path)
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err("GitHub did not confirm the change. Refresh and check before retrying.".into());
    }
    Ok(())
}

pub fn save_description(
    wt: &Path,
    previous: &Details,
    title: &str,
    body: &str,
) -> Result<(), String> {
    if title.trim().is_empty() {
        return Err("A pull request title cannot be empty".into());
    }
    if previous.target.remote().is_some() {
        return crate::azure::save_description(wt, previous, title, body);
    }
    let client = Client::new(wt, &previous.target);
    let current = fresh(&client, previous)?;
    if !current.can_update {
        return Err("You no longer have permission to edit this pull request".into());
    }
    if current.title != previous.title || current.body != previous.body {
        return Err("The PR description changed on GitHub. Your draft is kept; refresh and compare before saving.".into());
    }
    // shortcut: GitHub has no conditional update; use CAS if its API adds it to close this race.
    mutate(
        &client,
        "mutation($id:ID!,$title:String!,$body:String!){updatePullRequest(input:{pullRequestId:$id,title:$title,body:$body}){pullRequest{id}}}",
        &[("id", &current.id), ("title", title), ("body", body)],
        "/updatePullRequest/pullRequest/id",
    )
}

pub fn comment(wt: &Path, previous: &Details, body: &str) -> Result<(), String> {
    if body.trim().is_empty() {
        return Err("Write a comment before posting".into());
    }
    if previous.target.remote().is_some() {
        return crate::azure::comment(wt, previous, body);
    }
    let client = Client::new(wt, &previous.target);
    let current = fresh(&client, previous)?;
    if !current.can_comment {
        return Err("This conversation is locked".into());
    }
    mutate(
        &client,
        "mutation($id:ID!,$body:String!){addComment(input:{subjectId:$id,body:$body}){commentEdge{node{id}}}}",
        &[("id", &current.id), ("body", body)],
        "/addComment/commentEdge/node/id",
    )
}

fn fresh_thread(client: &Client, previous: &Details, id: &str) -> Result<Value, String> {
    if !previous.threads.iter().any(|t| t.id == id) {
        return Err("This thread does not belong to the displayed pull request".into());
    }
    let value = client.node(
        id,
        "PullRequestReviewThread",
        &format!("{THREAD} pullRequest{{id}}"),
        None,
    )?;
    if value["pullRequest"]["id"].as_str() != Some(&previous.id) {
        return Err("Review thread identity changed; refresh before continuing".into());
    }
    Ok(value)
}

pub fn reply(wt: &Path, previous: &Details, thread_id: &str, body: &str) -> Result<(), String> {
    if body.trim().is_empty() {
        return Err("Write a reply before posting".into());
    }
    if previous.target.remote().is_some() {
        return crate::azure::reply(wt, previous, thread_id, body);
    }
    let client = Client::new(wt, &previous.target);
    let value = fresh_thread(&client, previous, thread_id)?;
    if !boolean(&value, "viewerCanReply")? {
        return Err("You no longer have permission to reply to this thread".into());
    }
    mutate(
        &client,
        "mutation($id:ID!,$body:String!){addPullRequestReviewThreadReply(input:{pullRequestReviewThreadId:$id,body:$body}){comment{id}}}",
        &[("id", thread_id), ("body", body)],
        "/addPullRequestReviewThreadReply/comment/id",
    )
}

pub fn resolve(
    wt: &Path,
    previous: &Details,
    thread_id: &str,
    resolved: bool,
) -> Result<(), String> {
    if previous.target.remote().is_some() {
        return crate::azure::resolve(wt, previous, thread_id, resolved);
    }
    let client = Client::new(wt, &previous.target);
    let value = fresh_thread(&client, previous, thread_id)?;
    if boolean(&value, "isResolved")? == resolved {
        return Ok(());
    }
    let (field, permission) = if resolved {
        ("resolveReviewThread", "viewerCanResolve")
    } else {
        ("unresolveReviewThread", "viewerCanUnresolve")
    };
    if !boolean(&value, permission)? {
        return Err("You no longer have permission to change this thread's resolution".into());
    }
    mutate(
        &client,
        &format!("mutation($id:ID!){{{field}(input:{{threadId:$id}}){{thread{{id isResolved}}}}}}"),
        &[("id", thread_id)],
        &format!("/{field}/thread/id"),
    )
}

#[cfg(test)]
mod tests;
