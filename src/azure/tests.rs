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
    assert_eq!(
        Remote::parse("ssh://git@ssh.dev.azure.com:22/v3/o/p/r"),
        r("o", "p", "r")
    );
    assert_eq!(
        Remote::parse("https://dev.azure.com/o/v3/_git/r"),
        r("o", "v3", "r"),
        "a project named v3 isn't an ssh path"
    );
    for lookalike in [
        "git@ssh.dev.azure.com.example.org:v3/o/p/r",
        "git@evil-ssh.dev.azure.com:v3/o/p/r",
        "ssh://git@example.org/ssh.dev.azure.com/v3/o/p/r",
    ] {
        assert_eq!(Remote::parse(lookalike), None, "{lookalike}");
    }
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
