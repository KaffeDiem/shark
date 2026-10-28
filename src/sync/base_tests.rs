use super::{
    Job, Pr, PrState, Probed, Repo, open_pr, repo_diff_base, repos_from, run_job, status, worktrees,
};
use crate::git::{git, review_pr_url};
use crate::state::Workspace;
use serde_json::{Value, json};
use std::{ffi::OsString, fs, os::unix::fs::PermissionsExt, path::PathBuf};

struct Fixture {
    root: PathBuf,
    ws: Workspace,
    wt: PathBuf,
    path: OsString,
    head: String,
    main: String,
    release: String,
}

impl Fixture {
    fn new(name: &str) -> Self {
        crate::state::TEST_HOME
            .get_or_init(|| std::env::temp_dir().join(format!("shark-ui-{}", std::process::id())));
        let name = format!("sync-base-{name}-{}", std::process::id());
        let root = std::env::temp_dir().join(&name);
        let mut ws = Workspace {
            name,
            branch: "feature".into(),
            ..Default::default()
        };
        let wt = ws.dir().join("api");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&wt).unwrap();
        let remote = root.join("origin.git");
        git(&root, &["init", "-q", "--bare", &remote.to_string_lossy()]).unwrap();
        git(&wt, &["init", "-q", "-b", "main"]).unwrap();
        let commit = |message| {
            git(
                &wt,
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "commit",
                    "--allow-empty",
                    "-qm",
                    message,
                ],
            )
            .unwrap();
            git(&wt, &["rev-parse", "HEAD"]).unwrap()
        };
        let main = commit("main");
        git(&wt, &["checkout", "-qb", "release"]).unwrap();
        let release = commit("release");
        git(&wt, &["checkout", "-qb", "feature", "main"]).unwrap();
        let head = commit("feature");
        git(
            &wt,
            &["remote", "add", "origin", "https://github.com/o/api.git"],
        )
        .unwrap();
        git(
            &wt,
            &[
                "config",
                &format!("url.{}.insteadOf", remote.display()),
                "https://github.com/o/api.git",
            ],
        )
        .unwrap();
        git(&wt, &["push", "-q", "origin", "main", "release", "feature"]).unwrap();
        git(
            &wt,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        )
        .unwrap();
        git(&wt, &["update-ref", "refs/shark/pull/7/base", &main]).unwrap();
        git(
            &wt,
            &[
                "config",
                "branch.feature.shark-pr-url",
                "https://github.com/o/api/pull/7",
            ],
        )
        .unwrap();
        ws.repos.push(crate::state::WsRepo {
            name: "api".into(),
            src: wt.clone(),
            base: "refs/shark/pull/7/base".into(),
            branch: None,
        });
        let path = std::env::var_os("PATH").unwrap();
        let gh = root.join("gh");
        fs::write(
            &gh,
            format!(
                r#"#!/bin/sh
case "$PWD" in */'{name}'/*) ;; *) PATH='{path}' exec gh "$@" ;; esac
printf '%s\n' "$*" >> '{root}/calls'
case "$1 $2" in
"pr list")
  if [ -f '{root}/fail' ]; then echo 'lookup failed' >&2; exit 1; fi
  shift 2
  branch=
  while [ "$#" -gt 0 ]; do
    if [ "$1" = --head ]; then shift; branch=$1; fi
    shift
  done
  if [ "$branch" = feature ]; then cat '{root}/own.json'; else cat '{root}/parent.json'; fi ;;
"pr view") cat '{root}/review.json' ;;
*) exit 1 ;;
esac
"#,
                name = ws.name,
                root = root.display(),
                path = path.to_string_lossy(),
            ),
        )
        .unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        unsafe {
            std::env::set_var(
                "PATH",
                format!("{}:{}", root.display(), path.to_string_lossy()),
            )
        };
        let f = Self {
            root,
            ws,
            wt,
            path,
            head,
            main,
            release,
        };
        f.write("own.json", json!([]));
        f.write("parent.json", json!([]));
        f.write("review.json", f.pr(7, "main", "OPEN"));
        f
    }

    fn pr(&self, number: u64, base: &str, state: &str) -> Value {
        json!({
            "url": format!("https://github.com/o/api/pull/{number}"),
            "number": number, "state": state, "isCrossRepository": false,
            "headRefName": if number == 7 { "original" } else { "feature" },
            "headRefOid": self.head, "baseRefName": base,
            "mergeStateStatus": "CLEAN", "statusCheckRollup": [],
        })
    }

    fn write(&self, name: &str, value: Value) {
        fs::write(self.root.join(name), value.to_string()).unwrap();
    }

    fn job(&self, fetch: bool, pr: bool, prev: &[Repo]) -> Job {
        Job {
            repos: worktrees(&self.ws),
            fetch,
            pr,
            prev: prev.to_vec(),
            stack: vec![],
        }
    }

    fn repos(&self, p: Probed, last: &[Repo]) -> Vec<Repo> {
        repos_from(&self.ws, p.locals, p.prs, p.parents, last)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        unsafe { std::env::set_var("PATH", &self.path) };
        let _ = fs::remove_dir_all(self.ws.dir());
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn own_pr_replaces_review_identity_and_follows_retargets() {
    let _lock = crate::app::ui_tests::UI_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let f = Fixture::new("adopt");
    f.write("own.json", json!([f.pr(8, "release", "OPEN")]));
    // Work can continue locally after publishing the branch's own PR.
    git(
        &f.wt,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--allow-empty",
            "-qm",
            "unpublished work",
        ],
    )
    .unwrap();
    let local_head = git(&f.wt, &["rev-parse", "HEAD"]).unwrap();
    assert_ne!(local_head, f.head);
    let mut repos = f.repos(run_job(&f.job(false, true, &[])), &[]);
    assert_eq!(review_pr_url(&f.wt, "feature"), None);
    assert_eq!(repos[0].pr().unwrap().number, 8);
    assert_eq!(repos[0].base, "origin/release");
    let local = repos[0].local.as_ref().unwrap();
    assert!(!local.review);
    assert_eq!(local.base, (2, 1), "counts use the new PR's target");
    assert_eq!(repo_diff_base(&repos[0]).as_deref(), Some("origin/release"));
    assert!(
        fs::read_to_string(f.root.join("calls"))
            .unwrap()
            .contains("--repo https://github.com/o/api")
    );

    let local_only = f.repos(run_job(&f.job(false, false, &repos)), &repos);
    assert_eq!(local_only[0].base, "origin/release");
    assert_eq!(local_only[0].local.as_ref().unwrap().base, (2, 1));
    assert!(!local_only[0].local.as_ref().unwrap().review);

    repos[0].parent = Some(Pr {
        state: PrState::Merged,
        ..open_pr("api", 9)
    });
    f.write("own.json", json!([f.pr(8, "main", "OPEN")]));
    let changed = f.repos(run_job(&f.job(false, true, &repos)), &repos);
    assert_eq!(changed[0].base, "origin/main");
    assert_eq!(changed[0].local.as_ref().unwrap().base, (2, 0));
    assert_eq!(
        changed[0].parent, None,
        "a retarget must discard the old parent's merged state"
    );
    assert_eq!(git(&f.wt, &["rev-parse", "HEAD"]).unwrap(), local_head);

    f.write("own.json", json!([f.pr(8, "main", "CLOSED")]));
    assert_eq!(
        status::fetch_pr(&f.wt, "feature", &f.head)
            .unwrap()
            .unwrap()
            .number,
        8,
        "closing the adopted PR must not restore the original review identity"
    );
}

#[test]
fn review_identity_survives_absent_closed_fork_and_failed_own_pr_lookup() {
    let _lock = crate::app::ui_tests::UI_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let f = Fixture::new("keep-review");
    let mut fork = f.pr(8, "release", "OPEN");
    fork["isCrossRepository"] = json!(true);
    let mut upstream = f.pr(8, "release", "OPEN");
    upstream["url"] = json!("https://github.com/other/api/pull/8");
    let mut unrelated = f.pr(8, "release", "OPEN");
    unrelated["headRefOid"] = json!(f.release);
    let mut unknown = f.pr(8, "release", "OPEN");
    unknown["headRefOid"] = json!("0000000000000000000000000000000000000001");
    for own in [
        json!([]),
        json!([f.pr(8, "release", "CLOSED")]),
        json!([fork]),
        json!([upstream]),
        json!([unrelated]),
        json!([unknown]),
    ] {
        f.write("own.json", own);
        let repos = f.repos(run_job(&f.job(false, true, &[])), &[]);
        assert_eq!(repos[0].pr().unwrap().number, 7);
        assert!(repos[0].local.as_ref().unwrap().review);
        assert_eq!(repos[0].base, "refs/shark/pull/7/base");
        assert_eq!(repo_diff_base(&repos[0]), None);
        assert!(review_pr_url(&f.wt, "feature").unwrap().ends_with("/7"));
    }
    let review_url = "https://github.com/other/upstream/pull/7";
    git(
        &f.wt,
        &["config", "branch.feature.shark-pr-url", review_url],
    )
    .unwrap();
    let mut review = f.pr(7, "main", "OPEN");
    review["url"] = json!(review_url);
    f.write("review.json", review);
    fs::write(f.root.join("fail"), "").unwrap();
    let readable = status::fetch_pr(&f.wt, "feature", &f.head)
        .unwrap()
        .unwrap();
    assert_eq!(readable.number, 7);
    assert_eq!(
        readable.url, review_url,
        "an inaccessible origin must not hide the readable upstream review"
    );
    assert_eq!(review_pr_url(&f.wt, "feature").as_deref(), Some(review_url));

    fs::remove_file(f.root.join("review.json")).unwrap();
    assert!(status::fetch_pr(&f.wt, "feature", &f.head).is_err());
    assert_eq!(review_pr_url(&f.wt, "feature").as_deref(), Some(review_url));
}

#[test]
fn fetch_only_poll_refreshes_review_base_without_touching_work() {
    let _lock = crate::app::ui_tests::UI_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let f = Fixture::new("refresh-review");
    f.write("review.json", f.pr(7, "release", "OPEN"));
    fs::write(f.wt.join("unfinished.txt"), "keep this work\n").unwrap();
    let before = git(&f.wt, &["status", "--porcelain"]).unwrap();
    let p = run_job(&f.job(true, false, &[]));
    assert_eq!(p.fetch_error, None);
    assert_eq!(
        git(&f.wt, &["rev-parse", "refs/shark/pull/7/base"]).unwrap(),
        f.release
    );
    let repos = f.repos(p, &[]);
    assert!(repos[0].local.as_ref().unwrap().review);
    assert_eq!(repos[0].local.as_ref().unwrap().base, (1, 1));

    f.write("review.json", f.pr(7, "main", "OPEN"));
    let p = run_job(&f.job(true, false, &repos));
    assert_eq!(p.fetch_error, None);
    assert_eq!(
        git(&f.wt, &["rev-parse", "refs/shark/pull/7/base"]).unwrap(),
        f.main
    );
    assert_eq!(git(&f.wt, &["rev-parse", "HEAD"]).unwrap(), f.head);
    assert_eq!(git(&f.wt, &["status", "--porcelain"]).unwrap(), before);
    assert_eq!(
        fs::read_to_string(f.wt.join("unfinished.txt")).unwrap(),
        "keep this work\n"
    );
}

#[test]
fn changed_review_identity_does_not_promote_cached_pr_when_lookup_fails() {
    let _lock = crate::app::ui_tests::UI_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let f = Fixture::new("identity-race");
    let previous = f.repos(run_job(&f.job(false, true, &[])), &[]);
    let pr = previous[0]
        .pr()
        .unwrap_or_else(|| panic!("initial review lookup failed: {:?}", previous[0].pr));
    assert_eq!(pr.number, 7);
    assert!(previous[0].local.as_ref().unwrap().review);

    // Another probe adopted this branch's own PR, but its answer never reached this cache.
    git(&f.wt, &["config", "--unset", "branch.feature.shark-pr-url"]).unwrap();
    fs::write(f.root.join("fail"), "").unwrap();
    let current = f.repos(run_job(&f.job(false, true, &previous)), &previous);
    assert!(
        current[0].pr().is_none() || current[0].local.as_ref().unwrap().review,
        "the original review must not become this branch's own PR on lookup failure"
    );
    assert_eq!(repo_diff_base(&current[0]), None);
}

#[gpui_kit::test]
fn adopted_base_survives_closed_pr_polls(cx: &mut gpui_kit::TestAppContext) {
    use super::{GitSync, SyncEvent};
    use gpui_kit::AppContext as _;
    use std::{cell::RefCell, rc::Rc};
    let _lock = crate::app::ui_tests::UI_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    cx.update(gpui_kit::init);
    let f = Fixture::new("persist-adoption");
    f.write("own.json", json!([f.pr(8, "release", "OPEN")]));
    let (_, sync) = cx
        .update(|cx| {
            gpui_kit::open_window(gpui_kit::WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| GitSync::new(f.ws.clone(), window, cx))
            })
        })
        .unwrap();
    let events = Rc::new(RefCell::new(Vec::new()));
    let observed = events.clone();
    let _sub = cx.update(|cx| {
        cx.subscribe(&sync, move |_, event, _| {
            if let SyncEvent::PrBaseAdopted(moves) = event {
                observed.borrow_mut().push(moves.clone());
            }
        })
    });
    let p = run_job(&f.job(false, true, &[]));
    cx.update(|cx| {
        sync.update(cx, |s, cx| {
            s.apply(p, cx);
            assert_eq!(s.ws.repos[0].base, "origin/release");
            assert!(!s.repos[0].local.as_ref().unwrap().review);
        });
    });
    assert_eq!(
        *events.borrow(),
        vec![vec![(
            "api".to_string(),
            "refs/shark/pull/7/base".to_string(),
            "origin/release".to_string(),
        )]]
    );

    f.write("own.json", json!([f.pr(8, "release", "CLOSED")]));
    let job = cx.update(|cx| {
        let s = sync.read(cx);
        Job {
            prev: s.repos.clone(),
            ..Job::prs(&s.ws)
        }
    });
    let p = run_job(&job);
    cx.update(|cx| {
        sync.update(cx, |s, cx| {
            s.apply(p, cx);
            assert_eq!(s.ws.repos[0].base, "origin/release");
            assert_eq!(s.repos[0].base, "origin/release");
            assert_eq!(s.repos[0].local.as_ref().unwrap().base, (1, 1));
            assert_eq!(s.repos[0].pr().unwrap().state, PrState::Closed);
        });
    });
    assert_eq!(events.borrow().len(), 1, "adoption is persisted once");

    // A restart can happen after clearing review identity but before persisting the base.
    let (_, restarted) = cx
        .update(|cx| {
            gpui_kit::open_window(gpui_kit::WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| GitSync::new(f.ws.clone(), window, cx))
            })
        })
        .unwrap();
    let observed = events.clone();
    let _restart_sub = cx.update(|cx| {
        cx.subscribe(&restarted, move |_, event, _| {
            if let SyncEvent::PrBaseAdopted(moves) = event {
                observed.borrow_mut().push(moves.clone());
            }
        })
    });
    let p = run_job(&f.job(false, true, &[]));
    cx.update(|cx| {
        restarted.update(cx, |s, cx| {
            assert_eq!(s.ws.repos[0].base, "refs/shark/pull/7/base");
            s.apply(p, cx);
            assert_eq!(s.ws.repos[0].base, "origin/release");
            assert_eq!(s.repos[0].local.as_ref().unwrap().base, (1, 1));
            assert_eq!(s.repos[0].pr().unwrap().state, PrState::Closed);
        });
    });
    let events = events.borrow();
    assert_eq!(events.len(), 2, "restart recovers the pending adoption");
    assert_eq!(events[1], events[0]);
}

#[test]
fn cold_poll_preserves_retargeted_parent_boundary() {
    let _lock = crate::app::ui_tests::UI_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut f = Fixture::new("qa-parent-boundary");
    git(&f.wt, &["config", "--unset", "branch.feature.shark-pr-url"]).unwrap();
    f.ws.repos[0].base = "origin/main".into();
    let tree = git(&f.wt, &["rev-parse", "HEAD^{tree}"]).unwrap();
    let make_commit = |parent: &str, message: &str| {
        git(
            &f.wt,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit-tree",
                &tree,
                "-p",
                parent,
                "-m",
                message,
            ],
        )
        .unwrap()
    };
    let child = make_commit(&f.release, "child on original release");
    git(&f.wt, &["update-ref", "refs/heads/feature", &child]).unwrap();
    git(
        &f.wt,
        &["update-ref", "refs/remotes/origin/feature", &child],
    )
    .unwrap();
    let rewritten = make_commit(&f.main, "rewritten release");
    git(
        &f.wt,
        &["update-ref", "refs/remotes/origin/release", &rewritten],
    )
    .unwrap();
    let key = "branch.feature.shark-parent-tip";
    git(&f.wt, &["config", key, &f.release]).unwrap();
    let before = status::probe(&f.wt, "feature", "origin/release", None, true).unwrap();
    assert!(
        before.restack,
        "control: the retained boundary detects rewritten parent"
    );
    assert_eq!(before.parent_tip, f.release);
    let mut own = f.pr(8, "release", "OPEN");
    own["headRefOid"] = json!(child);
    f.write("own.json", json!([own]));
    let _quick = run_job(&f.job(false, false, &[]));
    assert_eq!(
        git(&f.wt, &["config", "--get", key]).unwrap(),
        f.release,
        "a cold quick poll has no authoritative target and must preserve the boundary"
    );
    let p = run_job(&f.job(false, true, &[]));
    let local = p.locals[0].as_ref().unwrap();
    assert!(
        local.restack,
        "cold poll lost rewritten-parent detection; parent_tip={:?}, persisted={:?}",
        local.parent_tip,
        git(&f.wt, &["config", "--get", key])
    );
    assert_eq!(local.parent_tip, f.release);
}

#[test]
fn fresh_review_reply_keeps_protection_after_concurrent_adoption() {
    let _lock = crate::app::ui_tests::UI_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let f = Fixture::new("fresh-identity-race");
    let script = fs::read_to_string(f.root.join("gh")).unwrap();
    fs::write(
        f.root.join("gh"),
        script.replace(
            "\"pr view\") cat",
            "\"pr view\") git config --unset branch.feature.shark-pr-url; cat",
        ),
    )
    .unwrap();
    let current = f.repos(run_job(&f.job(false, true, &[])), &[]);
    assert_eq!(review_pr_url(&f.wt, "feature"), None);
    assert_eq!(current[0].pr().unwrap().number, 7);
    assert!(
        current[0].local.as_ref().unwrap().review,
        "a fresh original review answer must not become this branch's own PR"
    );
    assert_eq!(repo_diff_base(&current[0]), None);
    assert!(super::check_review_checkout(&current, &[]).is_err());
    assert!(status::step_args(&super::Kind::Merge, &current[0]).is_err());
}

#[test]
fn remote_ahead_pr_is_adopted_only_for_the_published_upstream_branch() {
    let _lock = crate::app::ui_tests::UI_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for published in [false, true] {
        let f = Fixture::new(if published {
            "remote-ahead-owned"
        } else {
            "remote-ahead-namesake"
        });
        let tree = git(&f.wt, &["rev-parse", "HEAD^{tree}"]).unwrap();
        let remote_head = git(
            &f.wt,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit-tree",
                &tree,
                "-p",
                &f.head,
                "-m",
                "remote work after PR publication",
            ],
        )
        .unwrap();
        git(
            &f.wt,
            &[
                "push",
                "-q",
                "origin",
                &format!("{remote_head}:refs/heads/feature"),
            ],
        )
        .unwrap();
        // A teammate's push would leave this checkout's tracking ref stale.
        git(
            &f.wt,
            &["update-ref", "refs/remotes/origin/feature", &f.head],
        )
        .unwrap();
        git(
            &f.wt,
            &[
                "config",
                "remote.origin.fetch",
                "+refs/heads/main:refs/remotes/origin/main",
            ],
        )
        .unwrap();
        assert_eq!(
            git(&f.wt, &["rev-parse", "origin/feature"]).unwrap(),
            f.head
        );
        if published {
            git(&f.wt, &["config", "branch.feature.remote", "origin"]).unwrap();
            git(
                &f.wt,
                &["config", "branch.feature.merge", "refs/heads/feature"],
            )
            .unwrap();
        }
        let mut own = f.pr(8, "release", "OPEN");
        own["headRefOid"] = json!(remote_head);
        f.write("own.json", json!([own]));
        let current = f.repos(run_job(&f.job(true, true, &[])), &[]);
        assert_eq!(git(&f.wt, &["rev-parse", "HEAD"]).unwrap(), f.head);
        let expected_remote = if published { &remote_head } else { &f.head };
        assert_eq!(
            git(&f.wt, &["rev-parse", "origin/feature"])
                .unwrap()
                .as_str(),
            expected_remote.as_str()
        );
        assert_eq!(
            current[0].pr().unwrap().number,
            if published { 8 } else { 7 }
        );
        assert_eq!(current[0].local.as_ref().unwrap().review, !published);
        assert_eq!(review_pr_url(&f.wt, "feature").is_none(), published);
        if published {
            assert_eq!(current[0].local.as_ref().unwrap().remote, Some((0, 1)));
            assert_eq!(super::next_action(&current, &[]).kind, super::Kind::Pull);
        }
    }
}

#[test]
fn narrow_fetchspec_fetches_the_authoritative_pr_target() {
    let _lock = crate::app::ui_tests::UI_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let f = Fixture::new("narrow-fetch");
    let refspec = "+refs/heads/main:refs/remotes/origin/main";
    git(&f.wt, &["config", "remote.origin.fetch", refspec]).unwrap();
    git(&f.wt, &["update-ref", "-d", "refs/remotes/origin/release"]).unwrap();
    git(&f.wt, &["branch", "-D", "release"]).unwrap();
    f.write("own.json", json!([f.pr(8, "release", "OPEN")]));
    let p = run_job(&f.job(true, true, &[]));
    assert_eq!(p.fetch_error, None);
    let current = f.repos(p, &[]);
    assert_eq!(
        git(&f.wt, &["rev-parse", "origin/release"]).unwrap(),
        f.release
    );
    assert_eq!(current[0].base, "origin/release");
    assert_eq!(current[0].local.as_ref().unwrap().base, (1, 1));
    let mut effective = f.ws.clone();
    effective.repos[0].base = current[0].base.clone();
    assert!(crate::git::combined_diff(&effective, &f.ws.dir(), false).is_ok());
    assert_eq!(
        git(&f.wt, &["config", "--get", "remote.origin.fetch"]).unwrap(),
        refspec
    );
    assert_eq!(git(&f.wt, &["rev-parse", "HEAD"]).unwrap(), f.head);
}

#[test]
fn failed_fetch_of_new_target_does_not_use_trunk_counts_or_touch_work() {
    let _lock = crate::app::ui_tests::UI_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let f = Fixture::new("missing-target-fetch");
    git(
        &f.wt,
        &["push", "-q", "origin", "release:refs/heads/new-target"],
    )
    .unwrap();
    git(
        &f.wt,
        &["update-ref", "-d", "refs/remotes/origin/new-target"],
    )
    .unwrap();
    f.write("own.json", json!([f.pr(8, "new-target", "OPEN")]));
    fs::write(f.wt.join("unfinished.txt"), "preserve dirty work").unwrap();
    let before = git(&f.wt, &["status", "--porcelain"]).unwrap();
    fs::rename(f.root.join("origin.git"), f.root.join("origin.offline")).unwrap();
    let p = run_job(&f.job(true, true, &[]));
    assert!(p.fetch_error.is_some());
    assert!(
        p.locals[0].is_err(),
        "an unavailable known target must not silently compare against trunk"
    );
    assert_eq!(
        git(&f.wt, &["rev-parse", "refs/shark/pull/7/base"]).unwrap(),
        f.main
    );
    assert_eq!(git(&f.wt, &["rev-parse", "HEAD"]).unwrap(), f.head);
    assert_eq!(git(&f.wt, &["status", "--porcelain"]).unwrap(), before);
    assert_eq!(
        fs::read_to_string(f.wt.join("unfinished.txt")).unwrap(),
        "preserve dirty work"
    );
}

#[test]
fn deleted_parent_keeps_merged_and_closed_stack_recovery() {
    let _lock = crate::app::ui_tests::UI_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut f = Fixture::new("deleted-parent");
    git(&f.wt, &["config", "--unset", "branch.feature.shark-pr-url"]).unwrap();
    f.ws.repos[0].base = "origin/release".into();
    let tree = git(&f.wt, &["rev-parse", "HEAD^{tree}"]).unwrap();
    let child = git(
        &f.wt,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit-tree",
            &tree,
            "-p",
            &f.release,
            "-m",
            "child of deleted parent",
        ],
    )
    .unwrap();
    git(&f.wt, &["update-ref", "refs/heads/feature", &child]).unwrap();
    git(&f.wt, &["push", "-q", "--force", "origin", "feature"]).unwrap();
    git(&f.wt, &["push", "-q", "origin", "--delete", "release"]).unwrap();
    git(&f.wt, &["branch", "-D", "release"]).unwrap();
    let key = "branch.feature.shark-parent-tip";
    git(&f.wt, &["config", key, &f.release]).unwrap();
    let mut own = f.pr(8, "release", "OPEN");
    own["headRefOid"] = json!(child);
    f.write("own.json", json!([own]));
    for state in ["MERGED", "CLOSED"] {
        let mut parent = f.pr(9, "main", state);
        parent["headRefName"] = json!("release");
        parent["headRefOid"] = json!(f.release);
        f.write("parent.json", json!([parent]));
        let p = run_job(&f.job(true, true, &[]));
        assert!(
            p.fetch_error.is_some(),
            "the deleted target cannot be fetched"
        );
        let current = f.repos(p, &[]);
        assert!(
            current[0].local.is_ok(),
            "a known terminal parent must retain stack recovery"
        );
        assert_eq!(current[0].parent.as_ref().unwrap().number, 9);
        let action = super::next_action(&current, &[]);
        if state == "MERGED" {
            assert_eq!(action.kind, super::Kind::Restack);
        } else {
            assert_eq!(action.label, "Parent PR closed");
            assert_eq!(action.kind, super::Kind::None);
        }
        assert_eq!(git(&f.wt, &["config", "--get", key]).unwrap(), f.release);
        assert_eq!(git(&f.wt, &["rev-parse", "HEAD"]).unwrap(), child);
    }
}

#[test]
fn retarget_to_stale_trunk_keeps_boundary_until_squash_fetch() {
    let _lock = crate::app::ui_tests::UI_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut f = Fixture::new("retarget-before-fetch");
    git(&f.wt, &["config", "--unset", "branch.feature.shark-pr-url"]).unwrap();
    f.ws.repos[0].base = "origin/release".into();
    let tree = git(&f.wt, &["rev-parse", "HEAD^{tree}"]).unwrap();
    let commit = |parent: &str, message: &str| {
        git(
            &f.wt,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit-tree",
                &tree,
                "-p",
                parent,
                "-m",
                message,
            ],
        )
        .unwrap()
    };
    let child = commit(&f.release, "child of original parent");
    git(&f.wt, &["update-ref", "refs/heads/feature", &child]).unwrap();
    git(&f.wt, &["push", "-q", "--force", "origin", "feature"]).unwrap();
    let key = "branch.feature.shark-parent-tip";
    git(&f.wt, &["config", key, &f.release]).unwrap();
    let mut own = f.pr(8, "release", "OPEN");
    own["headRefOid"] = json!(child);
    f.write("own.json", json!([own.clone()]));
    let previous = f.repos(run_job(&f.job(false, true, &[])), &[]);

    let squashed = commit(&f.main, "parent squash merged into main");
    git(
        &f.wt,
        &[
            "push",
            "-q",
            "origin",
            &format!("{squashed}:refs/heads/main"),
        ],
    )
    .unwrap();
    git(&f.wt, &["update-ref", "refs/remotes/origin/main", &f.main]).unwrap();
    own["baseRefName"] = json!("main");
    f.write("own.json", json!([own]));
    let retargeted = f.repos(run_job(&f.job(false, true, &previous)), &previous);
    assert_eq!(retargeted[0].base, "origin/main");
    assert_eq!(
        git(&f.wt, &["config", "--get", key]).unwrap(),
        f.release,
        "fresh retarget metadata must not clear the boundary against an unfetched trunk"
    );
    let warm = f.repos(run_job(&f.job(false, false, &retargeted)), &retargeted);
    assert_eq!(
        git(&f.wt, &["config", "--get", key]).unwrap(),
        f.release,
        "a quick poll with cached main metadata must preserve the boundary too"
    );

    let fetched = run_job(&f.job(true, true, &warm));
    assert_eq!(fetched.fetch_error, None);
    let current = f.repos(fetched, &warm);
    assert_eq!(git(&f.wt, &["rev-parse", "origin/main"]).unwrap(), squashed);
    let local = current[0].local.as_ref().unwrap();
    assert_eq!(local.parent_tip, f.release);
    assert!(
        local.restack,
        "the original boundary must still detect the squash merge"
    );
    assert!(matches!(
        super::next_action(&current, &[]).kind,
        super::Kind::Agent(_)
    ));
    assert_eq!(git(&f.wt, &["rev-parse", "HEAD"]).unwrap(), child);
}

#[test]
fn reverse_identity_race_keeps_protection_when_probe_fails() {
    let _lock = crate::app::ui_tests::UI_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let f = Fixture::new("reverse-role-probe-error");
    git(&f.wt, &["config", "--unset", "branch.feature.shark-pr-url"]).unwrap();
    f.write("own.json", json!([f.pr(8, "release", "OPEN")]));
    let script = fs::read_to_string(f.root.join("gh")).unwrap();
    fs::write(f.root.join("gh"), script.replace(
        "if [ \"$branch\" = feature ]; then cat",
        "if [ \"$branch\" = feature ]; then git config branch.feature.shark-pr-url https://github.com/o/api/pull/7; git config status.showUntrackedFiles bogus; cat",
    )).unwrap();
    let current = f.repos(run_job(&f.job(false, true, &[])), &[]);
    assert!(current[0].local.is_err());
    assert_eq!(current[0].pr().unwrap().number, 8);
    assert!(current[0].pr().unwrap().review_checkout);
    assert!(current[0].is_review());
    assert!(super::check_review_checkout(&current, &[]).is_err());
    assert!(
        status::step_args(&super::Kind::Merge, &current[0])
            .unwrap_err()
            .contains("review checkout")
    );
    assert_eq!(repo_diff_base(&current[0]), None);
}

#[test]
fn probe_error_drops_cached_own_pr_after_review_identity_appears() {
    let _lock = crate::app::ui_tests::UI_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let f = Fixture::new("reverse-role-cache-error");
    git(&f.wt, &["config", "--unset", "branch.feature.shark-pr-url"]).unwrap();
    f.write("own.json", json!([f.pr(8, "release", "OPEN")]));
    let previous = f.repos(run_job(&f.job(false, true, &[])), &[]);
    assert!(!previous[0].is_review());
    git(
        &f.wt,
        &[
            "config",
            "branch.feature.shark-pr-url",
            "https://github.com/o/api/pull/7",
        ],
    )
    .unwrap();
    git(&f.wt, &["config", "status.showUntrackedFiles", "bogus"]).unwrap();
    fs::write(f.root.join("fail"), "").unwrap();
    fs::remove_file(f.root.join("review.json")).unwrap();
    let current = f.repos(run_job(&f.job(false, true, &previous)), &previous);
    assert!(current[0].local.is_err());
    assert!(
        current[0].pr().is_none(),
        "cached ordinary PR must not return after identity and local probe failure"
    );
}
