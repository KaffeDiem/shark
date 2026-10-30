use super::*;
use std::time::Duration;

#[test]
fn included_secrets_require_absent_ignored_destination() {
    use std::os::unix::fs::symlink;
    let root = std::env::temp_dir().join(format!("shark-secret-copy-{}", std::process::id()));
    let src = init_repo(&root, "src");
    let dst = init_repo(&root, "dst");
    fs::remove_file(dst.path.join(".env")).unwrap();
    copy_included(&src.path, &dst.path).unwrap();
    assert_eq!(fs::read(dst.path.join(".env")).unwrap(), b"SECRET=1\n");
    assert!(
        !git(&dst.path, &["status", "--porcelain"])
            .unwrap()
            .contains(".env\n")
    );
    fs::write(dst.path.join(".env"), "keep").unwrap();
    copy_included(&src.path, &dst.path).unwrap();
    assert_eq!(fs::read(dst.path.join(".env")).unwrap(), b"keep");
    fs::remove_file(dst.path.join(".env")).unwrap();
    let outside = root.join("outside");
    fs::write(&outside, "outside").unwrap();
    symlink(&outside, dst.path.join(".env")).unwrap();
    copy_included(&src.path, &dst.path).unwrap();
    assert_eq!(fs::read(&outside).unwrap(), b"outside");
    fs::remove_file(dst.path.join(".env")).unwrap();
    fs::create_dir(dst.path.join(".env")).unwrap();
    copy_included(&src.path, &dst.path).unwrap();
    assert!(dst.path.join(".env").is_dir());
    fs::remove_dir(dst.path.join(".env")).unwrap();
    fs::write(dst.path.join(".env"), "tracked").unwrap();
    git(&dst.path, &["add", "-f", ".env"]).unwrap();
    fs::remove_file(dst.path.join(".env")).unwrap();
    copy_included(&src.path, &dst.path).unwrap();
    assert!(
        !dst.path.join(".env").exists(),
        "tracked but missing is not a copy destination"
    );
    git(&dst.path, &["rm", "--cached", "-f", ".env"]).unwrap();
    fs::write(dst.path.join(".gitignore"), "").unwrap();
    copy_included(&src.path, &dst.path).unwrap();
    assert!(!dst.path.join(".env").exists());
    fs::write(src.path.join(".gitignore"), "nested/\n").unwrap();
    fs::write(src.path.join(".worktreeinclude"), "nested/\n").unwrap();
    fs::create_dir(src.path.join("nested")).unwrap();
    fs::write(src.path.join("nested/secret"), "secret").unwrap();
    fs::write(dst.path.join(".gitignore"), "nested/\n").unwrap();
    symlink(&root, dst.path.join("nested")).unwrap();
    assert!(copy_included(&src.path, &dst.path).is_err());
    assert!(!root.join("secret").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rollback_only_deletes_owned_unchanged_refs() {
    let root = std::env::temp_dir().join(format!("shark-owned-rollback-{}", std::process::id()));
    let repo = init_repo(&root, "api");
    git(&repo.path, &["branch", "existing"]).unwrap();
    assert!(undo_worktree(&root, "existing", &repo, &OwnedWorktree::default()).is_empty());
    assert!(has_ref(&repo.path, "refs/heads/existing"));
    let tip = git(&repo.path, &["rev-parse", "HEAD"]).unwrap();
    let owned = OwnedWorktree {
        branch: Some(tip.clone()),
        worktree: None,
        untracked: None,
    };
    git(&repo.path, &["branch", "owned"]).unwrap();
    assert!(undo_worktree(&root, "owned", &repo, &owned).is_empty());
    assert!(!has_ref(&repo.path, "refs/heads/owned"));
    git(&repo.path, &["branch", "advanced"]).unwrap();
    let owned = OwnedWorktree {
        branch: Some("0000000000000000000000000000000000000001".into()),
        worktree: None,
        untracked: None,
    };
    assert!(!undo_worktree(&root, "advanced", &repo, &owned).is_empty());
    assert_eq!(git(&repo.path, &["rev-parse", "advanced"]).unwrap(), tip);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rollback_preserves_changed_worktrees_and_cleanup_failures() {
    let root = std::env::temp_dir().join(format!("shark-owned-worktrees-{}", std::process::id()));
    let repo = init_repo(&root, "api");
    let dir = root.join("ws");
    fs::create_dir(&dir).unwrap();
    for (branch, change) in [
        ("dirty", 0),
        ("secret", 1),
        ("advanced", 2),
        ("locked", 3),
        ("foreign", 4),
    ] {
        let plan = plan_worktree(branch, &repo, None).unwrap();
        let mut owned = OwnedWorktree::default();
        add_worktree(&dir, branch, &repo, &plan, &mut owned).unwrap();
        let wt = dir.join("api");
        match change {
            0 => fs::write(wt.join("README.md"), "new edits").unwrap(),
            1 => fs::write(wt.join(".env"), "changed secret").unwrap(),
            2 => {
                commit(&wt, &["--allow-empty", "-qm", "advanced"]);
            }
            3 => {
                git(&repo.path, &["worktree", "lock", &wt.to_string_lossy()]).unwrap();
            }
            _ => {
                fs::write(repo.path.join(".git/info/exclude"), "foreign\n").unwrap();
                fs::write(wt.join("foreign"), "another writer's ignored file").unwrap();
            }
        }
        let remaining = undo_worktree(&dir, branch, &repo, &owned);
        assert!(remaining.iter().any(|p| p == &wt.display().to_string()));
        assert!(wt.exists() && has_ref(&repo.path, &format!("refs/heads/{branch}")));
        if change == 3 {
            git(&repo.path, &["worktree", "unlock", &wt.to_string_lossy()]).unwrap();
        }
        git(
            &repo.path,
            &["worktree", "remove", "--force", &wt.to_string_lossy()],
        )
        .unwrap();
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn base_choices_merge_newest_first() {
    let b = |t, n: &str| (t, format!("origin/{n}"));
    // web is on its own branch `x`; api's `feat` is still a base web may take.
    let repos = [
        (
            "api".to_string(),
            "feat".to_string(),
            vec![b(9, "feat"), b(5, "main"), b(1, "old")],
        ),
        (
            "web".to_string(),
            "x".to_string(),
            vec![b(8, "feat"), b(7, "main"), b(3, "x")],
        ),
    ];
    let got = base_choices(&repos);
    let names: Vec<_> = got
        .iter()
        .map(|(n, rs)| (n.as_str(), rs.join(",")))
        .collect();
    assert_eq!(
        names,
        [
            ("origin/feat", "web".into()),
            ("origin/main", "web,api".into()),
            ("origin/old", "api".into())
        ]
    );
}

/// A timeout kills the whole group: a background child holding stdout open dies too.
#[test]
fn group_timeout_kills_grandchildren() {
    let pidfile = std::env::temp_dir().join(format!("shark-grp-{}", std::process::id()));
    let script = format!("sleep 30 & echo $! > {}; wait", pidfile.display());
    let started = std::time::Instant::now();
    let c = Command::new("sh");
    let e =
        crate::proc::wait_bounded(c, Path::new("/"), "sh", &["-c", &script], 1, true).unwrap_err();
    assert!(
        e.contains("timed out") && started.elapsed().as_secs() < 5,
        "{e}"
    );
    let pid = fs::read_to_string(&pidfile).unwrap();
    // Killed at once, but `kill -0` sees it until launchd reaps the orphan.
    let alive = || crate::proc::signal(pid.trim().parse().unwrap(), 0);
    let reaped = (0..40).any(|_| {
        std::thread::sleep(Duration::from_millis(50));
        !alive()
    });
    assert!(reaped, "sleep {pid} outlived the timeout");
    fs::remove_file(&pidfile).ok();
}

fn init_repo(root: &Path, name: &str) -> Repo {
    let p = root.join("src").join(name);
    fs::create_dir_all(&p).unwrap();
    git(&p, &["init", "-q", "-b", "main"]).unwrap();
    fs::write(p.join("README.md"), "hi\n").unwrap();
    fs::write(p.join("CLAUDE.md"), format!("{name} rules\n")).unwrap();
    fs::write(p.join(".gitignore"), ".env\n").unwrap();
    fs::write(p.join(".env"), "SECRET=1\n").unwrap();
    git(&p, &["add", "."]).unwrap();
    fs::write(p.join(".env.local"), "untracked, not ignored\n").unwrap();
    commit(&p, &["-qm", "init"]);
    add_repo(&p, &[]).unwrap()
}

#[test]
fn pr_checkout_uses_exact_tip_base_and_rolls_back() {
    let root = std::env::temp_dir().join(format!("shark-pr-checkout-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = init_repo(&root, "api");
    let other = init_repo(&root, "web");
    let remote = root.join("upstream.git");
    git(&root, &["init", "-q", "--bare", &remote.to_string_lossy()]).unwrap();
    // The fork's namesake target differs from the PR repository's target.
    git(
        &repo.path,
        &["update-ref", "refs/remotes/origin/release", "HEAD"],
    )
    .unwrap();
    fs::write(repo.path.join("parent.txt"), "parent change\n").unwrap();
    git(&repo.path, &["add", "parent.txt"]).unwrap();
    commit(&repo.path, &["-qm", "parent"]);
    let base = git(&repo.path, &["rev-parse", "HEAD"]).unwrap();
    git(
        &repo.path,
        &[
            "push",
            "-q",
            &remote.to_string_lossy(),
            "HEAD:refs/heads/release",
        ],
    )
    .unwrap();
    fs::write(repo.path.join("child.txt"), "child change\n").unwrap();
    git(&repo.path, &["add", "child.txt"]).unwrap();
    commit(&repo.path, &["-q", "-m", "PR tip"]);
    let tip = git(&repo.path, &["rev-parse", "HEAD"]).unwrap();
    git(
        &repo.path,
        &[
            "push",
            "-q",
            &remote.to_string_lossy(),
            "HEAD:refs/pull/7/head",
        ],
    )
    .unwrap();
    git(
        &repo.path,
        &[
            "config",
            &format!("url.{}.insteadOf", remote.display()),
            "https://github.com/o/upstream.git",
        ],
    )
    .unwrap();
    let pr = PullRequest {
        number: 7,
        url: "https://github.com/o/upstream/pull/7".into(),
        base: "release".into(),
    };
    let picked = [(repo.clone(), "pr/7".into())];
    let prs = [(repo.name.clone(), pr.clone())];
    let dir = root.join("ws/review");
    let ws = create_workspace_with_prs(&dir, "review", "pr/7", &picked, &prs, &[]).unwrap();
    let wt = dir.join("api");
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]).unwrap(), tip);
    assert_eq!(git(&wt, &["rev-parse", &ws.repos[0].base]).unwrap(), base);
    assert_eq!(ws.repos[0].base, "refs/shark/pull/7/base");
    let patch = combined_diff(&ws, &dir, false).unwrap();
    assert!(patch.contains("+++ b/api/child.txt"), "{patch}");
    assert!(!patch.contains("parent.txt"), "{patch}");
    assert_eq!(review_pr_url(&wt, "pr/7"), Some(pr.url));
    assert_eq!(
        git(&repo.path, &["branch", "--show-current"]).unwrap(),
        "main"
    );
    let collision = root.join("ws/collision");
    assert!(
        create_workspace_with_prs(&collision, "collision", "pr/7", &picked, &prs, &[])
            .err()
            .unwrap()
            .contains("already exists")
    );
    assert!(!collision.exists());
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]).unwrap(), tip);
    let missing = [(
        repo.name.clone(),
        PullRequest {
            number: 404,
            url: "https://github.com/o/upstream/pull/404".into(),
            base: "release".into(),
        },
    )];
    let failed = root.join("ws/missing");
    assert!(
        create_workspace_with_prs(
            &failed,
            "missing",
            "pr/404",
            &[(repo.clone(), "pr/404".into())],
            &missing,
            &[]
        )
        .is_err()
    );
    assert!(!failed.exists() && !has_ref(&repo.path, "refs/heads/pr/404"));
    git(&other.path, &["branch", "blocked"]).unwrap();
    let failed = root.join("ws/rollback");
    let picked = [
        (repo.clone(), "review-again".into()),
        (other, "blocked/child".into()),
    ];
    assert!(
        create_workspace_with_prs(&failed, "rollback", "review-again", &picked, &prs, &[]).is_err()
    );
    assert!(!failed.exists() && !has_ref(&repo.path, "refs/heads/review-again"));
    assert_eq!(review_pr_url(&repo.path, "review-again"), None);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn review_base_refresh_follows_target_and_preserves_worktree() {
    let root = std::env::temp_dir().join(format!("shark-review-refresh-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = init_repo(&root, "api");
    let upstream = root.join("upstream.git");
    let fork = root.join("fork.git");
    let initial = git(&repo.path, &["rev-parse", "HEAD"]).unwrap();
    for (remote, url) in [
        (&upstream, "https://github.com/o/upstream.git"),
        (&fork, "https://github.com/me/fork.git"),
    ] {
        git(&root, &["init", "-q", "--bare", &remote.to_string_lossy()]).unwrap();
        git(
            &repo.path,
            &[
                "push",
                "-q",
                &remote.to_string_lossy(),
                "HEAD:refs/heads/release",
            ],
        )
        .unwrap();
        git(
            &repo.path,
            &[
                "config",
                &format!("url.{}.insteadOf", remote.display()),
                url,
            ],
        )
        .unwrap();
    }
    git(
        &repo.path,
        &["remote", "add", "origin", "https://github.com/me/fork.git"],
    )
    .unwrap();
    git(&repo.path, &["fetch", "-q", "origin"]).unwrap();
    git(
        &repo.path,
        &[
            "push",
            "-q",
            &upstream.to_string_lossy(),
            "HEAD:refs/pull/7/head",
            "HEAD:refs/heads/other",
        ],
    )
    .unwrap();
    let mut pr = PullRequest {
        number: 7,
        url: "https://github.com/o/upstream/pull/7".into(),
        base: "release".into(),
    };
    let dir = root.join("ws/review");
    let ws = create_workspace_with_prs(
        &dir,
        "review",
        "review-7",
        &[(repo.clone(), "review-7".into())],
        &[(repo.name.clone(), pr.clone())],
        &[],
    )
    .unwrap();
    let wt = dir.join("api");
    fs::write(wt.join("README.md"), "staged\n").unwrap();
    git(&wt, &["add", "README.md"]).unwrap();
    fs::write(wt.join("README.md"), "unstaged\n").unwrap();
    fs::write(wt.join("untracked.txt"), "keep me\n").unwrap();
    let status = git(&wt, &["status", "--porcelain"]).unwrap();
    let index = git(&wt, &["diff", "--cached"]).unwrap();
    commit(&repo.path, &["-qm", "advance", "--allow-empty"]);
    let advanced = git(&repo.path, &["rev-parse", "HEAD"]).unwrap();
    git(
        &repo.path,
        &[
            "push",
            "-q",
            &upstream.to_string_lossy(),
            "HEAD:refs/heads/release",
        ],
    )
    .unwrap();
    refresh_review_base(&wt, &pr).unwrap();
    assert_eq!(
        git(&wt, &["rev-parse", &ws.repos[0].base]).unwrap(),
        advanced
    );
    assert_eq!(git(&wt, &["rev-parse", "origin/release"]).unwrap(), initial);

    pr.base = "other".into();
    refresh_review_base(&wt, &pr).unwrap();
    assert_eq!(
        git(&wt, &["rev-parse", &ws.repos[0].base]).unwrap(),
        initial
    );
    pr.base = "missing".into();
    assert!(refresh_review_base(&wt, &pr).is_err());
    assert_eq!(
        git(&wt, &["rev-parse", &ws.repos[0].base]).unwrap(),
        initial
    );
    pr.base = "../invalid".into();
    assert!(refresh_review_base(&wt, &pr).is_err());
    pr.base = "release".into();
    pr.url = "https://github.com/o/upstream/pull/8".into();
    assert!(refresh_review_base(&wt, &pr).is_err());
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]).unwrap(), initial);
    assert_eq!(
        git(&wt, &["rev-parse", "refs/shark/pull/7/head"]).unwrap(),
        initial
    );
    assert_eq!(git(&wt, &["status", "--porcelain"]).unwrap(), status);
    assert_eq!(git(&wt, &["diff", "--cached"]).unwrap(), index);
    assert_eq!(
        fs::read_to_string(wt.join("README.md")).unwrap(),
        "unstaged\n"
    );
    assert_eq!(
        fs::read_to_string(wt.join("untracked.txt")).unwrap(),
        "keep me\n"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn stacked_diff_uses_target_merge_base_and_preserves_index() {
    let root = std::env::temp_dir().join(format!("shark-stacked-diff-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = init_repo(&root, "api");
    let commit_file = |at: &Path, file: &str| {
        fs::write(at.join(file), format!("{file}\n")).unwrap();
        git(at, &["add", file]).unwrap();
        commit(at, &["-qm", file]);
    };
    git(&repo.path, &["checkout", "-qb", "parent"]).unwrap();
    commit_file(&repo.path, "parent.txt");
    git(
        &repo.path,
        &["update-ref", "refs/remotes/origin/parent", "HEAD"],
    )
    .unwrap();
    let dir = root.join("ws/child");
    let mut ws = create_workspace_with_prs(
        &dir,
        "child",
        "child",
        &on_branch(std::slice::from_ref(&repo), "child"),
        &[],
        &[("api".into(), "parent".into())],
    )
    .unwrap();
    let wt = dir.join("api");
    commit_file(&wt, "child.txt");
    // Changes made only on the target must not appear as deletions in the PR.
    commit_file(&repo.path, "target-only.txt");
    git(
        &repo.path,
        &["update-ref", "refs/remotes/origin/parent", "HEAD"],
    )
    .unwrap();
    fs::write(wt.join("README.md"), "unstaged\n").unwrap();
    fs::write(wt.join("staged.txt"), "staged\n").unwrap();
    git(&wt, &["add", "staged.txt"]).unwrap();
    fs::write(wt.join("untracked.txt"), "untracked\n").unwrap();
    let index = git(
        &wt,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    )
    .unwrap();
    let before = fs::read(&index).unwrap();
    let patch = combined_diff(&ws, &dir, false).unwrap();
    for file in ["child.txt", "README.md", "staged.txt", "untracked.txt"] {
        assert!(patch.contains(&format!("+++ b/api/{file}")), "{patch}");
    }
    for file in ["parent.txt", "target-only.txt"] {
        assert!(!patch.contains(file), "{patch}");
    }
    let local = combined_diff(&ws, &dir, true).unwrap();
    assert!(!local.contains("child.txt"), "{local}");
    for file in ["README.md", "staged.txt", "untracked.txt"] {
        assert!(local.contains(&format!("+++ b/api/{file}")), "{local}");
    }
    assert_eq!(
        fs::read(&index).unwrap(),
        before,
        "diff leaves staging intact"
    );
    ws.repos[0].base = "origin/missing-target".into();
    assert!(
        combined_diff(&ws, &dir, false)
            .unwrap_err()
            .starts_with("api: git diff:")
    );
    assert_eq!(combined_diff(&ws, &dir, true).unwrap(), local);
    assert_eq!(
        fs::read(&index).unwrap(),
        before,
        "errors leave staging intact"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn workspace_roundtrip() {
    let root = std::env::temp_dir().join(format!("shark-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repos = vec![init_repo(&root, "api"), init_repo(&root, "web")];
    let dir = root.join("ws/feat");
    let ws = create_workspace(&dir, "feat", "feat-x", &on_branch(&repos, "feat-x")).unwrap();

    assert_eq!(
        git(&dir.join("api"), &["branch", "--show-current"]).unwrap(),
        "feat-x"
    );
    assert!(dir.join("web/.env").exists(), ".env copied");
    let md = fs::read_to_string(dir.join("CLAUDE.md")).unwrap();
    assert!(
        md.contains("@api/CLAUDE.md") && md.contains("@web/CLAUDE.md"),
        "{md}"
    );
    let agents = fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    assert!(
        agents.contains("read `api/CLAUDE.md`") && agents.contains("branch `feat-x`"),
        "{agents}"
    );
    assert!(
        create_workspace(
            &root.join("ws/other"),
            "other",
            "feat-x",
            &on_branch(&repos, "feat-x")
        )
        .is_err(),
        "branch collision"
    );
    // `git init` + `remote add` leaves origin/HEAD unset; the base is still origin/main.
    let origin = root.join("origin.git");
    git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]).unwrap();
    let api = &repos[0].path;
    git(api, &["remote", "add", "origin", &origin.to_string_lossy()]).unwrap();
    git(api, &["push", "-q", "origin", "main"]).unwrap();
    git(api, &["checkout", "-q", "-b", "side"]).unwrap();
    let other = create_workspace(
        &root.join("ws/other"),
        "other",
        "feat-y",
        &on_branch(&repos[..1], "feat-y"),
    )
    .unwrap();
    assert_eq!(other.repos[0].base, "origin/main");
    remove_workspace(&other, &root.join("ws/other"), true).unwrap();

    fs::write(dir.join("web/dirty.txt"), "x\n").unwrap();
    let err = remove_workspace(&ws, &dir, false).unwrap_err();
    assert!(err.contains("web") && !err.contains("api"), "{err}");
    assert!(
        dir.join("api/README.md").exists(),
        "clean api kept while web is dirty"
    );
    fs::remove_file(dir.join("web/dirty.txt")).unwrap();

    fs::write(dir.join("api/README.md"), "changed\n").unwrap();
    fs::write(dir.join("web/new.txt"), "new\n").unwrap();
    fs::write(dir.join("web/résumé.txt"), "new\n").unwrap();
    git(&dir.join("web"), &["mv", "CLAUDE.md", "RULES.md"]).unwrap();
    let diff = combined_diff(&ws, &dir, false).unwrap();
    assert!(diff.contains("+++ b/api/README.md"), "{diff}");
    assert!(
        diff.contains("--- a/web/CLAUDE.md") && diff.contains("+++ b/web/RULES.md"),
        "pure rename keeps the repo prefix: {diff}"
    );
    let mut bad = ws.clone();
    bad.repos[0].base = "origin/nope".into();
    assert!(
        combined_diff(&bad, &dir, false)
            .unwrap_err()
            .starts_with("api: ")
    );
    assert!(diff.contains("+++ b/web/new.txt"), "{diff}");
    assert!(diff.contains("b/web/r"), "{diff}");
    assert!(
        !dir.join("api/.env.local").exists(),
        "non-ignored files are not copied"
    );

    fs::write(dir.join("notes.md"), "mine\n").unwrap();
    assert!(
        remove_workspace(&ws, &dir, false).is_err(),
        "unexpected root file blocks non-forced archive"
    );
    assert!(dir.join("notes.md").exists());
    remove_workspace(&ws, &dir, true).unwrap();
    assert!(!dir.exists());
    assert!(
        !git(&repos[0].path, &["worktree", "list"])
            .unwrap()
            .contains("feat")
    );
    // Only a branch still at the merged head goes; a moved one (web) is kept.
    let head = |r: &Repo| git(&r.path, &["rev-parse", "feat-x"]).unwrap();
    let merged = [
        ("api".into(), head(&repos[0])),
        ("web".into(), "moved".into()),
    ];
    assert_eq!(delete_merged_branches(&ws, &merged), ["api"]);
    assert!(
        git(
            &repos[0].path,
            &["rev-parse", "--verify", "--quiet", "feat-x"]
        )
        .is_err()
    );
    assert!(
        git(
            &repos[1].path,
            &["rev-parse", "--verify", "--quiet", "feat-x"]
        )
        .is_ok()
    );
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn per_repo_branches() {
    let root = std::env::temp_dir().join(format!("shark-test-prb-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let (api, web) = (init_repo(&root, "api"), init_repo(&root, "web"));
    let origin = root.join("origin.git");
    git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]).unwrap();
    let a = &api.path;
    git(a, &["remote", "add", "origin", &origin.to_string_lossy()]).unwrap();
    git(a, &["push", "-q", "origin", "main", "main:kd/x"]).unwrap();
    git(a, &["remote", "set-head", "origin", "main"]).unwrap();
    // `behind`: origin has one commit more than the local branch.
    git(a, &["checkout", "-q", "-b", "behind"]).unwrap();
    commit(a, &["--allow-empty", "-m", "more"]);
    git(a, &["push", "-q", "origin", "behind"]).unwrap();
    let tip = git(a, &["rev-parse", "HEAD"]).unwrap();
    git(a, &["reset", "-q", "--hard", "HEAD~1"]).unwrap();
    git(a, &["checkout", "-q", "main"]).unwrap();

    // api on an origin-only branch, web on the new one.
    let dir = root.join("ws/feat");
    let ws = create_workspace(
        &dir,
        "feat",
        "feat",
        &[(api.clone(), "kd/x".into()), (web.clone(), "feat".into())],
    )
    .unwrap();
    let on = |r: &str| git(&dir.join(r), &["branch", "--show-current"]).unwrap();
    assert_eq!((on("api"), on("web")), ("kd/x".into(), "feat".into()));
    assert_eq!(ws.repos[0].branch.as_deref(), Some("kd/x"));
    assert_eq!(ws.repos[1].branch, None);
    let md = fs::read_to_string(dir.join("CLAUDE.md")).unwrap();
    assert!(
        md.contains("worktree:\n") && md.contains(" on `kd/x` ") && md.contains(" on `feat` "),
        "{md}"
    );

    let bs = branches(a).unwrap();
    let get = |n: &str| bs.iter().find(|b| b.name == n).cloned();
    let x = get("kd/x").unwrap();
    assert!(x.local && x.origin, "{x:?}");
    assert!(x.checked_out.unwrap().ends_with("ws/feat/api"));
    let main = get("main").unwrap();
    assert!(
        main.local && main.origin && main.checked_out.is_some(),
        "{main:?}"
    );
    assert!(get("HEAD").is_none(), "origin/HEAD is dropped");

    // A branch checked out in the source checkout is refused; nothing is left behind.
    let err = create_workspace(
        &root.join("ws/m"),
        "m",
        "m",
        &[(web.clone(), "m".into()), (api.clone(), "main".into())],
    )
    .err()
    .unwrap();
    assert!(
        err.contains("api: branch `main` is already checked out"),
        "{err}"
    );
    assert!(!root.join("ws/m").exists());
    assert!(
        git(
            &web.path,
            &["rev-parse", "--verify", "--quiet", "refs/heads/m"]
        )
        .is_err()
    );

    // A local branch behind origin is fast-forwarded first.
    let b = root.join("ws/b");
    let bws = create_workspace(
        &b,
        "b",
        "behind",
        &on_branch(std::slice::from_ref(&api), "behind"),
    )
    .unwrap();
    assert_eq!(git(&b.join("api"), &["rev-parse", "HEAD"]).unwrap(), tip);
    remove_workspace(&bws, &b, true).unwrap();

    // Archive deletes each repo's own branch.
    remove_workspace(&ws, &dir, true).unwrap();
    let x_head = git(a, &["rev-parse", "kd/x"]).unwrap();
    let merged = [("api".into(), x_head), ("web".into(), "moved".into())];
    assert_eq!(delete_merged_branches(&ws, &merged), ["api"]);
    assert!(git(a, &["rev-parse", "--verify", "--quiet", "refs/heads/kd/x"]).is_err());
    assert!(
        git(
            &web.path,
            &["rev-parse", "--verify", "--quiet", "refs/heads/feat"]
        )
        .is_ok()
    );
    fs::remove_dir_all(&root).unwrap();
}

/// A new branch stacked on a parent starts at `origin/<parent>` (never the local parent's
/// unpushed commits), targets it, and records it as its parent tip; another repo of the same
/// workspace stays on its trunk. An unpushed or missing parent (a fork's PR) or an existing
/// branch name refuses and leaves nothing behind.
#[test]
fn stacks_new_branches_on_a_parent() {
    let root = std::env::temp_dir().join(format!("shark-test-stack-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let (api, web) = (init_repo(&root, "api"), init_repo(&root, "web"));
    let empty_commit = |dir: &Path, msg: &str| {
        commit(dir, &["--allow-empty", "-m", msg]);
        git(dir, &["rev-parse", "HEAD"]).unwrap()
    };
    for r in [&api, &web] {
        let origin = root.join(format!("{}.git", r.name));
        git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]).unwrap();
        (git(
            &r.path,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        ))
        .unwrap();
        git(&r.path, &["push", "-q", "origin", "main"]).unwrap();
        git(&r.path, &["remote", "set-head", "origin", "main"]).unwrap();
    }
    let a = &api.path;
    let branch = |name: &str, f: &dyn Fn()| {
        git(a, &["checkout", "-q", "-b", name, "main"]).unwrap();
        f();
        git(a, &["checkout", "-q", "main"]).unwrap();
    };
    // Only on origin.
    let mut remote_tip = String::new();
    branch("remote", &|| {
        empty_commit(a, "remote");
    });
    git(a, &["push", "-q", "origin", "remote"]).unwrap();
    remote_tip.push_str(&git(a, &["rev-parse", "remote"]).unwrap());
    git(a, &["branch", "-q", "-D", "remote"]).unwrap();
    // Local is ahead of origin: your own parent workspace with an unpushed commit.
    branch("ahead", &|| {
        empty_commit(a, "pushed");
        git(a, &["push", "-q", "origin", "ahead"]).unwrap();
        empty_commit(a, "unpushed");
    });
    // Local is behind origin: someone pushed to the parent since.
    branch("behind", &|| {
        empty_commit(a, "old");
        empty_commit(a, "new");
        git(a, &["push", "-q", "origin", "behind"]).unwrap();
        git(a, &["reset", "-q", "--hard", "HEAD~1"]).unwrap();
    });
    // Never pushed.
    branch("local", &|| {
        empty_commit(a, "local");
    });
    let head = |dir: &Path| git(dir, &["rev-parse", "HEAD"]).unwrap();
    let cases = [
        ("remote", remote_tip.clone()),
        ("ahead", git(a, &["rev-parse", "origin/ahead"]).unwrap()),
        ("behind", git(a, &["rev-parse", "origin/behind"]).unwrap()),
    ];
    for (parent, tip) in cases {
        let name = format!("on-{parent}");
        let dir = root.join("ws").join(&name);
        let parents = [("api".to_string(), parent.to_string())];
        let ws = create_workspace_with_prs(
            &dir,
            &name,
            &name,
            &on_branch(&[api.clone(), web.clone()], &name),
            &[],
            &parents,
        )
        .unwrap();
        assert_eq!(head(&dir.join("api")), tip, "{parent}");
        let key = parent_tip_key(&name);
        assert_eq!(git(a, &["config", &key]).unwrap(), tip);
        assert_eq!(ws.repos[0].base, format!("origin/{parent}"));
        assert_eq!(ws.repos[1].base, "origin/main", "web isn't stacked");
        assert_eq!(head(&dir.join("web")), head(&web.path));
    }
    for (parent, name, want) in [
        ("fork-only", "on-fork", "`fork-only` isn't a branch here"),
        (
            "local",
            "on-local",
            "`local` isn't on GitHub yet; push it first",
        ),
        (
            "remote",
            "ahead",
            "`ahead` already exists; stacking on `remote` needs a new",
        ),
    ] {
        let dir = root.join("ws").join(name);
        let parents = [("api".to_string(), parent.to_string())];
        let picked = on_branch(&[web.clone(), api.clone()], name);
        let err = create_workspace_with_prs(&dir, name, name, &picked, &[], &parents)
            .err()
            .unwrap();
        assert!(err.contains(want), "{err}");
        assert!(!dir.exists());
        assert!(!has_ref(&web.path, &format!("refs/heads/{name}")));
    }
    let _ = fs::remove_dir_all(&root);
}

/// The edge cases of picking existing branches per repo.
#[test]
fn per_repo_branch_edges() {
    let root = std::env::temp_dir().join(format!("shark-test-pre-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let (api, web) = (init_repo(&root, "api"), init_repo(&root, "web"));
    let empty_commit = |dir: &Path, msg: &str| {
        commit(dir, &["--allow-empty", "-m", msg]);
        git(dir, &["rev-parse", "HEAD"]).unwrap()
    };
    for r in [&api, &web] {
        let origin = root.join(format!("{}.git", r.name));
        git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]).unwrap();
        git(
            &r.path,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        )
        .unwrap();
        git(&r.path, &["push", "-q", "origin", "main", "main:shared"]).unwrap();
    }
    let a = &api.path;

    // Diverged: local and origin each have a commit; local is kept, not reset.
    git(a, &["checkout", "-q", "-b", "div"]).unwrap();
    let theirs = empty_commit(a, "theirs");
    git(a, &["push", "-q", "origin", "div"]).unwrap();
    git(a, &["reset", "-q", "--hard", "HEAD~1"]).unwrap();
    let mine = empty_commit(a, "mine");
    git(a, &["checkout", "-q", "main"]).unwrap();
    let d = root.join("ws/d");
    let dws = create_workspace(
        &d,
        "d",
        "div",
        &on_branch(std::slice::from_ref(&api), "div"),
    )
    .unwrap();
    assert_eq!(git(&d.join("api"), &["rev-parse", "HEAD"]).unwrap(), mine);
    assert_eq!(git(a, &["rev-parse", "origin/div"]).unwrap(), theirs);
    remove_workspace(&dws, &d, true).unwrap();

    // The same existing branch in both repos, under another workspace name: each checks
    // out its own, and CLAUDE.md names the one they share.
    let s = root.join("ws/s");
    let mut sws = create_workspace(
        &s,
        "s",
        "s",
        &[
            (api.clone(), "shared".into()),
            (web.clone(), "shared".into()),
        ],
    )
    .unwrap();
    for r in ["api", "web"] {
        let b = git(&s.join(r), &["branch", "--show-current"]).unwrap();
        assert_eq!(b, "shared");
    }
    assert!(
        sws.repos
            .iter()
            .all(|r| r.branch.as_deref() == Some("shared"))
    );
    let md = fs::read_to_string(s.join("CLAUDE.md")).unwrap();
    assert!(
        md.contains("worktree on branch `shared`:") && !md.contains(" on `"),
        "{md}"
    );
    // A repo added later gets the workspace branch, and CLAUDE.md now tells them apart.
    let cli = init_repo(&root, "cli");
    sws = add_to_workspace(&sws, &s, &cli).unwrap();
    assert_eq!(
        git(&s.join("cli"), &["branch", "--show-current"]).unwrap(),
        "s"
    );
    assert_eq!(sws.repos[2].branch, None);
    let md = fs::read_to_string(s.join("CLAUDE.md")).unwrap();
    assert!(
        md.contains(" on `shared` ") && md.contains(" on `s` "),
        "{md}"
    );
    remove_workspace(&sws, &s, true).unwrap();

    // A ref clash (`kd` exists, `kd/y` can't): everything is rolled back.
    git(a, &["branch", "kd"]).unwrap();
    let c = root.join("ws/c");
    let err = create_workspace(
        &c,
        "c",
        "c",
        &[(web.clone(), "c".into()), (api.clone(), "kd/y".into())],
    )
    .err()
    .unwrap();
    assert!(err.contains("kd/y"), "{err}");
    assert!(!c.exists());
    let has = |r: &Repo, b: &str| {
        git(
            &r.path,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{b}"),
            ],
        )
        .is_ok()
    };
    assert!(!has(&web, "c") && !has(&api, "kd/y"));
    for r in [&api, &web] {
        let wts = git(&r.path, &["worktree", "list"]).unwrap();
        assert_eq!(wts.lines().count(), 1, "{wts}");
    }

    // An invalid name for any repo is refused before anything happens.
    let err = create_workspace(
        &root.join("ws/bad"),
        "bad",
        "ok",
        &[
            (web.clone(), "ok".into()),
            (api.clone(), "bad..name".into()),
        ],
    )
    .err()
    .unwrap();
    assert!(err.contains("bad..name"), "{err}");
    assert!(!has(&web, "ok"));
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn readable_add_repo_errors() {
    let dir = std::env::temp_dir().join(format!("shark-test-norepo-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let e = add_repo(&dir, &[]).err().unwrap();
    assert_eq!(e, format!("Not a git repository: {}", dir.display()));
    let e = run(&dir, "shark-no-such-prog", &[]).unwrap_err();
    assert_eq!(e, "shark-no-such-prog not found on PATH");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn branch_reuse_add_repo_and_blockers() {
    let root = std::env::temp_dir().join(format!("shark-test-br-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repos = vec![init_repo(&root, "api"), init_repo(&root, "web")];
    let origin = root.join("origin.git");
    git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]).unwrap();
    let api = &repos[0].path;
    git(api, &["remote", "add", "origin", &origin.to_string_lossy()]).unwrap();
    git(api, &["push", "-q", "origin", "main", "main:remote-only"]).unwrap();
    git(api, &["branch", "local-only"]).unwrap();

    // Origin-only: reused from origin, without tracking it.
    let dir = root.join("ws/r");
    let on = on_branch(&repos[..1], "remote-only");
    let ws = create_workspace(&dir, "r", "remote-only", &on).unwrap();
    let wt = dir.join("api");
    assert_eq!(
        git(&wt, &["branch", "--show-current"]).unwrap(),
        "remote-only"
    );
    assert!(
        git(&wt, &["rev-parse", "--abbrev-ref", "@{u}"]).is_err(),
        "--no-track"
    );

    // Local-only (an archived workspace's leftover): refused, and nothing is left behind.
    let on = on_branch(&repos, "local-only");
    let err = create_workspace(&root.join("ws/l"), "l", "local-only", &on)
        .err()
        .unwrap();
    assert!(err.contains("api: a local branch"), "{err}");
    assert!(!root.join("ws/l").exists());
    assert!(
        git(
            &repos[1].path,
            &["rev-parse", "--verify", "--quiet", "refs/heads/local-only"]
        )
        .is_err()
    );

    // Local-only with unpushed commits: live work, reused.
    git(api, &["checkout", "-q", "-b", "unpushed"]).unwrap();
    commit(api, &["-q", "--allow-empty", "-m", "wip"]);
    let wip = git(api, &["rev-parse", "HEAD"]).unwrap();
    git(api, &["checkout", "-q", "main"]).unwrap();
    let u = root.join("ws/u");
    let uws = create_workspace(&u, "u", "unpushed", &on_branch(&repos[..1], "unpushed")).unwrap();
    assert_eq!(git(&u.join("api"), &["rev-parse", "HEAD"]).unwrap(), wip);
    remove_workspace(&uws, &u, true).unwrap();

    // A third repo joins on the same branch; CLAUDE.md lists it.
    let ws = add_to_workspace(&ws, &dir, &repos[1]).unwrap();
    assert_eq!(
        git(&dir.join("web"), &["branch", "--show-current"]).unwrap(),
        "remote-only"
    );
    assert!(
        fs::read_to_string(dir.join("CLAUDE.md"))
            .unwrap()
            .contains("@web/CLAUDE.md")
    );
    assert!(
        add_to_workspace(&ws, &dir, &repos[1]).is_err(),
        "already in it"
    );

    // A locked worktree blocks archive up front: the other repo stays.
    git(api, &["worktree", "lock", &wt.to_string_lossy()]).unwrap();
    for force in [false, true] {
        let err = remove_workspace(&ws, &dir, force).unwrap_err();
        assert!(err.contains("api: locked"), "{err}");
        assert!(dir.join("web/README.md").exists());
    }
    git(api, &["worktree", "unlock", &wt.to_string_lossy()]).unwrap();
    fs::create_dir_all(dir.join("web/sub/.git")).unwrap();
    git(
        &dir.join("web"),
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},sub", git(api, &["rev-parse", "HEAD"]).unwrap()),
        ],
    )
    .unwrap();
    let err = remove_workspace(&ws, &dir, false).unwrap_err();
    assert!(err.contains("web: has submodules"), "{err}");
    assert!(dir.join("api/README.md").exists());
    remove_workspace(&ws, &dir, true).unwrap();
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn default_base_skips_pruned_origin_head() {
    let root = std::env::temp_dir().join(format!("shark-pruned-head-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = init_repo(&root, "api");
    let origin = root.join("origin.git");
    git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]).unwrap();
    let api = &repo.path;
    // No origin: the current branch is no trunk, but a new workspace still gets a base.
    assert_eq!(default_base(api), "");
    let dir = root.join("ws/feat");
    let ws = create_workspace(
        &dir,
        "feat",
        "feat",
        &on_branch(std::slice::from_ref(&repo), "feat"),
    )
    .unwrap();
    assert_eq!(ws.repos[0].base, "main");
    assert_eq!(
        default_base(&dir.join("api")),
        "",
        "not the workspace's own branch"
    );
    git(api, &["remote", "add", "origin", &origin.to_string_lossy()]).unwrap();
    git(api, &["push", "-q", "origin", "main"]).unwrap();
    git(api, &["fetch", "-q", "origin"]).unwrap();
    git(api, &["remote", "set-head", "origin", "main"]).unwrap();
    assert_eq!(default_base(api), "origin/main");
    // The remote's default was renamed and `fetch --prune` removed the old ref.
    git(
        api,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/master",
        ],
    )
    .unwrap();
    assert_eq!(default_base(api), "origin/main");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn archive_refusal_is_preflighted_and_tolerates_finder_and_moved_source() {
    let root = std::env::temp_dir().join(format!("shark-archive-pre-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = init_repo(&root, "api");
    let dir = root.join("ws/feat");
    let ws = create_workspace(
        &dir,
        "feat",
        "feat",
        &on_branch(std::slice::from_ref(&repo), "feat"),
    )
    .unwrap();
    fs::write(dir.join("api/dirty.txt"), "x\n").unwrap();
    let err = archive_preflight(&ws, &dir, false).unwrap_err();
    assert!(err.contains("uncommitted changes in: api"), "{err}");
    archive_preflight(&ws, &dir, true).unwrap();
    fs::remove_file(dir.join("api/dirty.txt")).unwrap();
    fs::write(dir.join("notes.md"), "mine\n").unwrap();
    assert!(archive_preflight(&ws, &dir, false).is_err());
    fs::remove_file(dir.join("notes.md")).unwrap();
    // Finder litter alone doesn't block a clean archive.
    fs::write(dir.join(".DS_Store"), "x").unwrap();
    archive_preflight(&ws, &dir, false).unwrap();
    remove_workspace(&ws, &dir, false).unwrap();
    assert!(!dir.exists());

    // Force still removes the workspace when its source repo was moved away.
    let dir = root.join("ws/feat2");
    let ws = create_workspace(
        &dir,
        "feat2",
        "feat2",
        &on_branch(std::slice::from_ref(&repo), "feat2"),
    )
    .unwrap();
    fs::rename(&repo.path, root.join("moved")).unwrap();
    assert!(remove_workspace(&ws, &dir, false).is_err());
    assert!(dir.join("api/README.md").exists());
    remove_workspace(&ws, &dir, true).unwrap();
    assert!(!dir.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn hand_deleted_workspace_dir_frees_its_branch() {
    let root = std::env::temp_dir().join(format!("shark-prunable-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = init_repo(&root, "api");
    let dir = root.join("ws/feat");
    create_workspace(
        &dir,
        "feat",
        "feat",
        &on_branch(std::slice::from_ref(&repo), "feat"),
    )
    .unwrap();
    let checked_out = || {
        let bs = branches(&repo.path).unwrap();
        bs.into_iter()
            .find(|b| b.name == "feat")
            .unwrap()
            .checked_out
    };
    assert!(checked_out().is_some());
    fs::remove_dir_all(&dir).unwrap();
    assert_eq!(
        checked_out(),
        None,
        "prunable entry doesn't hold the branch"
    );
    create_workspace(&dir, "feat", "feat", &on_branch(&[repo], "feat")).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn default_env_pattern_skips_ignored_directories() {
    let root = std::env::temp_dir().join(format!("shark-env-dirs-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let src = init_repo(&root, "src");
    let dst = init_repo(&root, "dst");
    let ignore = ".env\n.env.prod\nnode_modules\n";
    for r in [&src, &dst] {
        fs::write(r.path.join(".gitignore"), ignore).unwrap();
    }
    fs::remove_file(src.path.join(".env")).unwrap();
    fs::remove_file(dst.path.join(".env")).unwrap();
    for f in [
        ".env/lib/mod.py",
        "node_modules/x/.env.example",
        "pkg/.env.prod",
        ".env.prod",
    ] {
        let p = src.path.join(f);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "v\n").unwrap();
    }
    let copied = copy_included(&src.path, &dst.path).unwrap();
    assert_eq!(copied.len(), 2, "{copied:?}");
    assert!(dst.path.join(".env.prod").exists() && dst.path.join("pkg/.env.prod").exists());
    assert!(!dst.path.join(".env").exists());
    assert!(!dst.path.join("node_modules").exists());
    // A configured `.worktreeinclude` keeps its own reach.
    fs::write(src.path.join(".worktreeinclude"), ".env.example\n").unwrap();
    copy_included(&src.path, &dst.path).unwrap();
    assert!(dst.path.join("node_modules/x/.env.example").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pr_remote_matches_origin_case_insensitively() {
    let root = std::env::temp_dir().join(format!("shark-pr-remote-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = init_repo(&root, "api");
    git(
        &repo.path,
        &["remote", "add", "origin", "git@GitHub.com:Owner/Repo.git"],
    )
    .unwrap();
    let pr = |url: &str| PullRequest {
        number: 3,
        url: url.into(),
        base: "main".into(),
    };
    let same = pr("https://github.com/owner/repo/pull/3");
    assert_eq!(pr_remote(&repo.path, &same).unwrap(), "origin");
    let other = pr("https://github.com/owner/fork/pull/3");
    assert_eq!(
        pr_remote(&repo.path, &other).unwrap(),
        "https://github.com/owner/fork.git"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_bytes_keeps_whitespace_and_git_trims() {
    let dir = std::env::temp_dir().join(format!("shark-git-bytes-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    assert_eq!(
        git_bytes(&dir, &["--version"]).map(|b| b.ends_with(b"\n")),
        Ok(true)
    );
    git(&dir, &["init", "-q"]).unwrap();
    fs::write(dir.join("f"), "  x\n").unwrap();
    git(&dir, &["add", "f"]).unwrap();
    commit(&dir, &["-m", "i"]);
    let raw = git_bytes(&dir, &["show", "HEAD:f"]).unwrap();
    assert_eq!(raw, b"  x\n");
    assert_eq!(git(&dir, &["show", "HEAD:f"]).unwrap(), "x");
    assert!(
        git_bytes(&dir, &["rev-parse", "nope"])
            .unwrap_err()
            .starts_with("git rev-parse nope")
    );
    fs::remove_dir_all(&dir).ok();
}
