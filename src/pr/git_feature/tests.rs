use super::*;
use serde_json::json;

fn target() -> Target {
    Target::from_url("https://github.com/owner/repo/pull/12").unwrap()
}

fn connection(nodes: Vec<Value>) -> Value {
    json!({"nodes":nodes,"pageInfo":{"hasNextPage":false,"endCursor":null}})
}

fn note(id: &str) -> Value {
    json!({"id":id,"body":"Comment **body**","author":{"login":"reviewer"},"url":"https://github.com/owner/repo/pull/12#discussion","createdAt":"2026-01-01T00:00:00Z"})
}

fn thread(id: &str) -> Value {
    json!({"id":id,"path":"src/main.rs","line":null,"isResolved":false,"isOutdated":true,"viewerCanReply":true,"viewerCanResolve":true,"viewerCanUnresolve":false,"pullRequest":{"id":"PR_12"},"comments":connection(vec![note("TC_1")])})
}

fn pr() -> Value {
    let mut review = note("R_1");
    review["state"] = json!("CHANGES_REQUESTED");
    json!({"id":"PR_12","number":12,"url":target().url(),"title":"Title","body":"Description\n","state":"OPEN","isDraft":false,"author":null,"headRefName":"feature","baseRefName":"main","viewerCanUpdate":true,"locked":false,"comments":connection(vec![note("C_1")]),"reviews":connection(vec![review]),"reviewThreads":connection(vec![thread("T_1")])})
}

#[test]
fn target_rejects_ambiguous_or_unsafe_urls() {
    for url in [
        "http://github.com/a/b/pull/1",
        "https://github.com/a/b/pull/0",
        "https://github.com/a/b/pull/1?x=y",
        "https://github.com/a/../pull/1",
        "https://user@github.com/a/b/pull/1",
        "https://github.com/a/b/pull/1/files",
        "https://github.com/a/b/pull/--help",
        "https://github.com/a/b/pull/1\n",
        "https://github.example.org:0/a/b/pull/1",
        "https://github.example.org:65536/a/b/pull/1",
        "https://github.example.org:+443/a/b/pull/1",
        "https://github.example.org:/a/b/pull/1",
        "https://github.example.org:443:443/a/b/pull/1",
        "https://github.com/a/b:other/pull/1",
    ] {
        assert!(Target::from_url(url).is_err(), "{url}");
    }
    assert_eq!(
        Target::from_url("https://GITHUB.com/owner/repo/pull/12/").unwrap(),
        target()
    );
    assert_eq!(
        Target::from_url("https://github.example.org/owner/repo/pull/12")
            .unwrap()
            .host,
        "github.example.org"
    );
    assert_eq!(
        Target::from_url("https://GITHUB.example.org:8443/owner/repo/pull/12")
            .unwrap()
            .url(),
        "https://github.example.org:8443/owner/repo/pull/12"
    );
}

#[test]
fn metadata_fails_closed_on_wrong_identity_and_missing_permissions() {
    let data = pr();
    let details = details_from(&target(), &data).unwrap();
    assert_eq!(details.author, "ghost");
    assert_eq!(details.body, "Description\n");
    for (key, val) in [
        ("number", json!(3)),
        ("url", json!("https://github.com/other/repo/pull/12")),
        ("url", json!("https://github.com/owner/other/pull/12")),
        (
            "url",
            json!("https://github.example.org/owner/repo/pull/12"),
        ),
        ("url", json!("https://github.com/owner/repo/pull/13")),
        ("viewerCanUpdate", Value::Null),
        ("locked", Value::Null),
    ] {
        let mut changed = data.clone();
        changed[key] = val;
        assert!(details_from(&target(), &changed).is_err(), "{key}");
    }
    let mut locked = data;
    locked["locked"] = json!(true);
    locked["viewerCanUpdate"] = json!(false);
    assert!(!details_from(&target(), &locked).unwrap().can_comment);
}

#[test]
fn metadata_accepts_canonical_repository_casing() {
    let requested = Target::from_url("https://github.com/OWNER/REPO/pull/12").unwrap();
    let details = details_from(&requested, &pr()).unwrap();
    assert_eq!(details.url, target().url());
    assert_eq!(details.target, requested);
    assert_eq!(
        details.target.url(),
        "https://github.com/OWNER/REPO/pull/12"
    );
}

#[test]
fn pagination_deadline_stops_before_spawning_another_process() {
    let target = target();
    let client = Client {
        wt: Path::new("."),
        target: &target,
        started: Instant::now() - Duration::from_secs(121),
    };
    assert!(
        client
            .query("query{viewer{login}}", &[], false)
            .unwrap_err()
            .contains("took too long")
    );
}

/// Opt-in authenticated read only; never creates, edits, replies, or resolves anything.
#[test]
#[ignore = "requires SHARK_GIT_FEATURE_LIVE_PR and authenticated gh in the target repository"]
fn live_read_only_pull_request() {
    #[cfg(feature = "test-support")]
    let _turn = crate::app::ui_tests::UI_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(
        std::env::var_os("SHARK_GH").is_none(),
        "live test requires the real gh executable"
    );
    let url = std::env::var("SHARK_GIT_FEATURE_LIVE_PR").expect("set an explicit live PR URL");
    let target = Target::from_url(&url).unwrap();
    let wt = std::env::current_dir().unwrap();
    let start = Instant::now();
    let details = load(&wt, &target).unwrap();
    assert_eq!(details.target, target);
    assert!(!details.id.is_empty());
    let discovered = discover(&wt, &details.head).unwrap();
    assert!(
        discovered.is_some(),
        "the live PR branch must be discoverable in this repository"
    );
    eprintln!(
        "Live Git read: PR #{}, {} conversation comments, {} reviews, {} threads, {} replies; load + discovery {:?}",
        details.target.number,
        details.comments.len(),
        details.reviews.len(),
        details.threads.len(),
        details
            .threads
            .iter()
            .map(|t| t.comments.len())
            .sum::<usize>(),
        start.elapsed()
    );
}

#[cfg(feature = "test-support")]
mod cli {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

    struct Fixture {
        root: PathBuf,
        previous: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Fixture {
        fn new() -> Self {
            let lock = crate::app::ui_tests::UI_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "shark-git-feature-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            let script = root.join("gh");
            fs::write(&script, r#"#!/usr/bin/env python3
import json, pathlib, sys
p = pathlib.Path('.')
args = sys.argv[1:]
with (p/'calls').open('a') as f: f.write(json.dumps(args)+'\n')
if args[:2] == ['pr','list']:
    print((p/'list.json').read_text()); sys.exit(0)
fields = dict(arg.split('=', 1) for arg in args if '=' in arg)
query = fields.get('query', '')
if query.count('$cursor') == 1:
    print('Unused GraphQL cursor variable', file=sys.stderr); sys.exit(1)
mode = (p/'mode').read_text() if (p/'mode').exists() else ''
if query.startswith('mutation'):
    if mode == 'http-error':
        print('HTTP 502: Bad Gateway', file=sys.stderr); sys.exit(1)
    if mode == 'invalid-json': print('invalid'); sys.exit(0)
    if mode == 'graphql-error': print(json.dumps({'data':None,'errors':[{'message':'Permission denied'}]})); sys.exit(0)
    for field in ['updatePullRequest','addComment','addPullRequestReviewThreadReply','unresolveReviewThread','resolveReviewThread']:
        if field+'(' in query:
            result = {'pullRequest':{'id':'PR_12'},'comment':{'id':'NEW'},'commentEdge':{'node':{'id':'NEW'}},'thread':{'id':fields['id'],'isResolved':field=='resolveReviewThread'}}
            print(json.dumps({'data':{field:result}})); sys.exit(0)
if 'node(id:' in query:
    if 'after:$cursor' in query:
        value = json.loads((p/('page-'+fields['id']+'.json')).read_text())
    else: value = json.loads((p/'thread.json').read_text())
    print(json.dumps({'data':{'node':value}})); sys.exit(0)
print(json.dumps({'data':{'repository':{'pullRequest':json.loads((p/'pr.json').read_text())}}}))
"#).unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
            let previous = std::env::var_os("SHARK_GH");
            unsafe {
                std::env::set_var("SHARK_GH", &script);
            }
            let fixture = Self {
                root,
                previous,
                _lock: lock,
            };
            fixture.write("pr.json", &pr());
            fixture.write("thread.json", &thread("T_1"));
            fixture
        }

        fn write(&self, file: &str, value: &Value) {
            fs::write(self.root.join(file), value.to_string()).unwrap();
        }
        fn mode(&self, mode: &str) {
            fs::write(self.root.join("mode"), mode).unwrap();
        }
        fn calls(&self) -> Vec<Vec<String>> {
            fs::read_to_string(self.root.join("calls"))
                .unwrap_or_default()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect()
        }
        fn mutations(&self) -> usize {
            self.calls()
                .iter()
                .filter(|args| args.iter().any(|a| a.starts_with("query=mutation")))
                .count()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(v) => std::env::set_var("SHARK_GH", v),
                    None => std::env::remove_var("SHARK_GH"),
                }
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn shared_gh_requests_redact_bodies_and_bound_timeouts() {
        let f = Fixture::new();
        let script = f.root.join("gh");
        fs::write(
            &script,
            "#!/bin/sh\nprintf 'warning\\nHTTP 502: Bad Gateway\\n' >&2\nexit 1\n",
        )
        .unwrap();
        let args = ["pr", "comment", "12", "--body", "private\nrequest body"];
        assert_eq!(
            crate::pr::gh_request(&f.root, &args, 30).unwrap_err(),
            "HTTP 502: Bad Gateway"
        );
        fs::write(&script, "#!/bin/sh\nexec sleep 10\n").unwrap();
        let started = Instant::now();
        let error = crate::pr::gh_request(&f.root, &args, 1).unwrap_err();
        assert_eq!(
            error,
            "timed out after 1s; it may have posted, so check the PR before retrying"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn discovery_pins_origin_and_preserves_explicit_review_identity() {
        let f = Fixture::new();
        crate::git::git(&f.root, &["init", "-q"]).unwrap();
        crate::git::git(
            &f.root,
            &[
                "config",
                "remote.origin.url",
                "git@github.com:owner/repo.git",
            ],
        )
        .unwrap();
        f.write("list.json", &json!([]));
        assert_eq!(discover(&f.root, "feature").unwrap(), None);
        f.write("list.json", &json!([
            {"url":"https://github.com/owner/repo/pull/14","state":"OPEN","isCrossRepository":true},
            {"url":"https://github.com/owner/repo/pull/13","state":"MERGED","isCrossRepository":false},
            {"url":target().url(),"state":"OPEN","isCrossRepository":false}
        ]));
        assert_eq!(discover(&f.root, "feature").unwrap(), Some(target()));
        assert!(
            f.calls()[0]
                .windows(2)
                .any(|a| a == ["--repo", "https://github.com/owner/repo"])
        );
        crate::git::git(
            &f.root,
            &[
                "config",
                "branch.feature.shark-pr-url",
                "https://github.com/upstream/repo/pull/9",
            ],
        )
        .unwrap();
        let calls = f.calls().len();
        assert_eq!(
            discover(&f.root, "feature").unwrap().unwrap().owner,
            "upstream"
        );
        assert_eq!(
            f.calls().len(),
            calls,
            "explicit review URL avoids branch ambiguity"
        );
    }

    #[test]
    fn loads_nested_pages_and_deduplicates_moving_connections() {
        let f = Fixture::new();
        let mut initial = pr();
        initial["comments"]["pageInfo"] = json!({"hasNextPage":true,"endCursor":"next-pr"});
        initial["reviews"]["pageInfo"] = json!({"hasNextPage":true,"endCursor":"next-reviews"});
        initial["reviewThreads"]["pageInfo"] =
            json!({"hasNextPage":true,"endCursor":"next-threads"});
        initial["reviewThreads"]["nodes"][0]["comments"]["pageInfo"] =
            json!({"hasNextPage":true,"endCursor":"next-thread"});
        f.write("pr.json", &initial);
        f.write(
            "page-PR_12.json",
            &json!({"comments":connection(vec![note("C_1"),note("C_2")]), "reviews":connection(vec![initial["reviews"]["nodes"][0].clone()]), "reviewThreads":connection(vec![thread("T_1"),thread("T_2")])}),
        );
        f.write(
            "page-T_1.json",
            &json!({"comments":connection(vec![note("TC_2")])}),
        );
        let loaded = load(&f.root, &target()).unwrap();
        assert_eq!(loaded.comments.len(), 2);
        assert_eq!(loaded.threads[0].comments.len(), 2);
        assert!(loaded.threads[0].outdated);
        assert_eq!(loaded.threads[0].line, None);
        assert_eq!(loaded.reviews[0].state, "CHANGES_REQUESTED");
        assert_eq!(loaded.reviews.len(), 1);
        assert_eq!(loaded.threads.len(), 2);
        assert_eq!(f.calls().len(), 5);
        let mut stuck = connection(vec![]);
        stuck["pageInfo"] = json!({"hasNextPage":true,"endCursor":"next-pr"});
        f.write("page-PR_12.json", &json!({"comments":stuck}));
        assert!(
            load(&f.root, &target())
                .unwrap_err()
                .contains("did not advance")
        );
    }

    #[test]
    fn mutations_pin_identity_recheck_permissions_and_preserve_literal_bodies() {
        let f = Fixture::new();
        let details = load(&f.root, &target()).unwrap();
        let body = "@secret\n$(touch should-not-exist) `literal` **markdown**";
        save_description(&f.root, &details, "New title", body).unwrap();
        comment(&f.root, &details, body).unwrap();
        reply(&f.root, &details, "T_1", body).unwrap();
        resolve(&f.root, &details, "T_1", true).unwrap();
        assert_eq!(f.mutations(), 4);
        assert!(!f.root.join("should-not-exist").exists());
        assert!(
            f.calls()
                .iter()
                .filter(|a| a
                    .iter()
                    .any(|a| a.starts_with("query=mutation") && !a.contains("resolveReviewThread")))
                .all(|a| a.contains(&format!("body={body}")))
        );
        assert!(reply(&f.root, &details, "FOREIGN_THREAD", "hi").is_err());
        let mut changed = pr();
        changed["body"] = json!("Someone else's update");
        f.write("pr.json", &changed);
        assert!(
            save_description(&f.root, &details, "New title", body)
                .unwrap_err()
                .contains("changed on GitHub")
        );
        changed["body"] = json!("Description\n");
        changed["viewerCanUpdate"] = json!(false);
        changed["locked"] = json!(true);
        f.write("pr.json", &changed);
        assert!(save_description(&f.root, &details, "New title", body).is_err());
        assert!(comment(&f.root, &details, body).is_err());
        let mut denied = thread("T_1");
        denied["viewerCanReply"] = json!(false);
        denied["viewerCanResolve"] = json!(false);
        f.write("thread.json", &denied);
        assert!(reply(&f.root, &details, "T_1", body).is_err());
        assert!(resolve(&f.root, &details, "T_1", true).is_err());
        denied["pullRequest"]["id"] = json!("OTHER_PR");
        f.write("thread.json", &denied);
        assert!(
            resolve(&f.root, &details, "T_1", true)
                .unwrap_err()
                .contains("identity changed")
        );
        assert_eq!(f.mutations(), 4, "failed preflight must never write");
    }

    #[test]
    fn ambiguous_writes_are_never_retried_and_resolution_is_idempotent() {
        let f = Fixture::new();
        let details = load(&f.root, &target()).unwrap();
        for mode in ["http-error", "invalid-json", "graphql-error"] {
            f.mode(mode);
            assert!(
                comment(&f.root, &details, "note")
                    .unwrap_err()
                    .contains("check before retrying")
            );
        }
        assert_eq!(f.mutations(), 3);
        resolve(&f.root, &details, "T_1", false).unwrap();
        assert_eq!(f.mutations(), 3, "already unresolved is a no-op");
        assert!(comment(&f.root, &details, " \n").is_err());
        assert!(reply(&f.root, &details, "T_1", "\t").is_err());
        assert!(save_description(&f.root, &details, " ", "").is_err());
        assert_eq!(f.mutations(), 3);
    }

    #[test]
    fn large_conversation_loads_without_per_thread_subprocesses() {
        let f = Fixture::new();
        let mut data = pr();
        data["reviewThreads"] = connection((0..2000).map(|i| thread(&format!("T_{i}"))).collect());
        f.write("pr.json", &data);
        let start = Instant::now();
        let details = load(&f.root, &target()).unwrap();
        assert_eq!(details.threads.len(), 2000);
        assert_eq!(
            f.calls().len(),
            1,
            "nested first pages share the metadata request"
        );
        assert!(start.elapsed() < Duration::from_secs(10));
        eprintln!("Loaded 2,000 review threads in {:?}", start.elapsed());
    }

    #[test]
    fn pagination_rejects_missing_cursor_and_skips_deleted_nodes() {
        let f = Fixture::new();
        let mut data = pr();
        data["comments"] = connection(vec![Value::Null, note("C_1"), note("C_1")]);
        f.write("pr.json", &data);
        assert_eq!(load(&f.root, &target()).unwrap().comments.len(), 1);
        for cursor in [Value::Null, json!("")] {
            data["comments"]["pageInfo"] = json!({"hasNextPage":true,"endCursor":cursor});
            f.write("pr.json", &data);
            assert!(
                load(&f.root, &target())
                    .unwrap_err()
                    .contains("cursor is missing")
            );
        }
        assert_eq!(
            f.calls().len(),
            3,
            "invalid cursors must not launch another request"
        );
    }

    #[test]
    fn refreshed_pr_node_identity_blocks_every_pr_mutation() {
        let f = Fixture::new();
        let details = load(&f.root, &target()).unwrap();
        let mut changed = pr();
        changed["id"] = json!("PR_REPLACEMENT");
        f.write("pr.json", &changed);
        assert!(
            save_description(&f.root, &details, "New title", "body")
                .unwrap_err()
                .contains("identity changed")
        );
        assert!(
            comment(&f.root, &details, "comment")
                .unwrap_err()
                .contains("identity changed")
        );
        assert_eq!(f.mutations(), 0);
    }

    #[test]
    fn reopening_threads_rechecks_its_own_permission() {
        let f = Fixture::new();
        let details = load(&f.root, &target()).unwrap();
        let mut current = thread("T_1");
        current["isResolved"] = json!(true);
        current["viewerCanUnresolve"] = json!(false);
        f.write("thread.json", &current);
        assert!(resolve(&f.root, &details, "T_1", false).is_err());
        assert_eq!(f.mutations(), 0);
        current["viewerCanResolve"] = json!(false);
        current["viewerCanUnresolve"] = json!(true);
        f.write("thread.json", &current);
        resolve(&f.root, &details, "T_1", false).unwrap();
        assert_eq!(f.mutations(), 1);
        assert!(
            f.calls()
                .last()
                .unwrap()
                .iter()
                .any(|arg| arg.starts_with("query=mutation")
                    && arg.contains("unresolveReviewThread"))
        );
    }
}
